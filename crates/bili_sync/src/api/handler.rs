use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{Extension, Json, Path, Query};
use axum::http::{header::IF_NONE_MATCH, HeaderMap};
use chrono::{DateTime, Datelike, Utc};
use html_escape::decode_html_entities;

use crate::http::headers::{create_api_headers, create_image_headers};
use crate::utils::time_format::{now_standard_string, to_standard_string};
use bili_sync_entity::{collection, favorite, page, submission, video, video_source, watch_later};
use bili_sync_migration::Expr;
use reqwest;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, FromQueryResult, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait, Unchanged,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};
use utoipa::OpenApi;

use crate::api::auth::OpenAPIAuth;
use crate::api::error::InnerApiError;
use crate::api::request::{
    AddVideoSourceRequest, BatchUpdateConfigRequest, ConfigHistoryRequest, ConfigMigrationRequest, QRGenerateRequest,
    QRPollRequest, ResetSpecificTasksRequest, ResetVideoSourcePathRequest, SetupAuthTokenRequest,
    SubmissionVideosRequest, UpdateConfigItemRequest, UpdateConfigRequest, UpdateCredentialRequest,
    UpdateVideoStatusRequest, VideosRequest,
};
use crate::api::response::{
    AddVideoSourceResponse, BangumiSeasonInfo, BangumiSourceListResponse, BangumiSourceOption,
    BetaImageUpdateStatusResponse, ConfigChangeInfo, ConfigHistoryResponse, ConfigItemResponse,
    ConfigMigrationReportResponse, ConfigMigrationStatusResponse, ConfigReloadResponse, ConfigResponse,
    ConfigValidationResponse, DashBoardResponse, DeleteVideoResponse, DeleteVideoSourceResponse,
    HotReloadStatusResponse, InitialSetupCheckResponse, MonitoringStatus, PageInfo, QRGenerateResponse, QRPollResponse,
    QRUserInfo, ResetAllVideosResponse, ResetVideoResponse, ResetVideoSourcePathResponse, SetupAuthTokenResponse,
    SubmissionVideosResponse, UpdateConfigResponse, UpdateCredentialResponse, UpdateVideoStatusResponse, VideoInfo,
    VideoResponse, VideoSource, VideoSourcesResponse, VideosResponse,
};
use crate::api::wrapper::{ApiError, ApiResponse};
use crate::utils::status::{PageStatus, VideoStatus};

// 全局静态的扫码登录服务实例
use once_cell::sync::Lazy;
static QR_SERVICE: Lazy<crate::auth::QRLoginService> = Lazy::new(crate::auth::QRLoginService::new);

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

static BETA_IMAGE_UPDATE_CACHE: Lazy<RwLock<Option<(DateTime<Utc>, BetaImageUpdateStatusResponse)>>> =
    Lazy::new(|| RwLock::new(None));
const BETA_IMAGE_UPDATE_CACHE_TTL_SECONDS: i64 = 10 * 60;
const IMAGE_PROXY_CACHE_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;
const IMAGE_PROXY_CACHE_CLEANUP_INTERVAL_SECONDS: i64 = 6 * 60 * 60;
const IMAGE_PROXY_CACHE_CONTROL: &str = "public, max-age=86400, stale-while-revalidate=604800";
static IMAGE_PROXY_CACHE_LAST_CLEANUP_AT: Lazy<RwLock<Option<DateTime<Utc>>>> = Lazy::new(|| RwLock::new(None));

const CNB_REGISTRY_BASE: &str = "https://docker.cnb.cool";
const CNB_REGISTRY_TOKEN_ENDPOINT: &str = "https://docker.cnb.cool/service/token";
const CNB_REGISTRY_SERVICE: &str = "cnb-registry";
const CNB_BILI_SYNC_REPOSITORY: &str = "sviplk.com/docker/bili-sync";
const CNB_BETA_TAG: &str = "beta";
const CNB_LATEST_TAG: &str = "latest";
const CNB_PACKAGES_PAGE_URL: &str = "https://cnb.cool/sviplk.com/docker/-/packages/docker/docker/bili-sync";
const BILI_SYNC_RELEASE_CHANNEL_ENV: &str = "BILI_SYNC_RELEASE_CHANNEL";
const BILI_SYNC_RELEASE_CHANNEL_FILE: &str = "/app/release-channel.txt";
const BILI_SYNC_RELEASE_CHANNEL_BUILT: Option<&str> = option_env!("BILI_SYNC_RELEASE_CHANNEL_BUILT");

fn normalize_release_channel(value: &str) -> Option<String> {
    let raw = value.trim();
    if raw.is_empty() {
        return None;
    }

    let lowered = raw.to_lowercase();
    let normalized = match lowered.as_str() {
        "beta" | "test" | "testing" => "beta",
        "stable" | "release" | "latest" => "stable",
        "dev" | "debug" => "dev",
        _ => lowered.as_str(),
    };
    Some(normalized.to_string())
}

fn get_release_channel() -> String {
    // 1) 运行时环境变量（允许用户手动覆盖）
    if let Ok(value) = std::env::var(BILI_SYNC_RELEASE_CHANNEL_ENV) {
        if let Some(normalized) = normalize_release_channel(&value) {
            return normalized;
        }
    }

    // 2) Docker 镜像内标记文件（Dockerfile 写入，不需要用户设置环境变量）
    if let Ok(value) = std::fs::read_to_string(BILI_SYNC_RELEASE_CHANNEL_FILE) {
        if let Some(normalized) = normalize_release_channel(&value) {
            return normalized;
        }
    }

    // 3) 编译期写入（CI 构建二进制时注入）
    if let Some(value) = BILI_SYNC_RELEASE_CHANNEL_BUILT {
        if let Some(normalized) = normalize_release_channel(value) {
            return normalized;
        }
    }

    // 4) 默认值
    if cfg!(debug_assertions) {
        "dev".to_string()
    } else {
        "stable".to_string()
    }
}

fn get_checked_tag(release_channel: &str) -> &'static str {
    match release_channel {
        "stable" => CNB_LATEST_TAG,
        // beta / dev / 其他都按测试通道走
        _ => CNB_BETA_TAG,
    }
}

fn extract_next_data_json(html: &str) -> Option<&str> {
    let marker = "id=\"__NEXT_DATA__\"";
    let marker_index = html.find(marker)?;
    let after_marker = &html[marker_index..];
    let tag_close_index = after_marker.find('>')?;
    let json_start = marker_index + tag_close_index + 1;
    let json_end = html[json_start..].find("</script>")? + json_start;
    Some(&html[json_start..json_end])
}

fn find_tag_push_at<'a>(value: &'a serde_json::Value, tag_name: &str) -> Option<&'a str> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(tags) = map.get("tags").and_then(|v| v.as_array()) {
                for tag in tags {
                    if tag.get("name").and_then(|v| v.as_str()) == Some(tag_name) {
                        if let Some(push_at) = tag
                            .get("last_pusher")
                            .and_then(|v| v.get("push_at"))
                            .and_then(|v| v.as_str())
                        {
                            return Some(push_at);
                        }

                        if let Some(push_at) = tag.get("push_at").and_then(|v| v.as_str()) {
                            return Some(push_at);
                        }
                    }
                }
            }

            map.values().find_map(|v| find_tag_push_at(v, tag_name))
        }
        serde_json::Value::Array(values) => values.iter().find_map(|v| find_tag_push_at(v, tag_name)),
        _ => None,
    }
}

#[cfg(test)]
mod beta_image_update_tests {
    use super::*;

    #[test]
    fn test_extract_next_data_json_and_find_tag_push_at_from_last_pusher() {
        let html = r#"
            <html>
              <script id="__NEXT_DATA__" type="application/json">
                {"props":{"pageProps":{"package":{"tags":[{"name":"beta","last_pusher":{"push_at":"2026-01-21T16:11:50.924+08:00"}}]}}}}
              </script>
            </html>
        "#;

        let json = extract_next_data_json(html).expect("应能提取 __NEXT_DATA__ JSON");
        let value: serde_json::Value = serde_json::from_str(json).expect("__NEXT_DATA__ 应为合法 JSON");
        let push_at = find_tag_push_at(&value, "beta").expect("应能找到 beta push_at");
        assert_eq!(push_at, "2026-01-21T16:11:50.924+08:00");
    }

    #[test]
    fn test_find_tag_push_at_from_direct_field() {
        let json = r#"{"tags":[{"name":"beta","push_at":"2026-01-21T16:11:50.924+08:00"}]}"#;
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let push_at = find_tag_push_at(&value, "beta").unwrap();
        assert_eq!(push_at, "2026-01-21T16:11:50.924+08:00");
    }

    #[test]
    fn test_get_local_built_time_utc_uses_env_override() {
        let key = "BILI_SYNC_IMAGE_BUILT_AT";
        let original = std::env::var(key).ok();

        std::env::set_var(key, "2026-01-21T09:02:18Z");
        let dt = get_local_built_time_utc().expect("应能获取本地构建时间");
        assert_eq!(dt.to_rfc3339(), "2026-01-21T09:02:18+00:00");

        match original {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

fn parse_http_date_to_utc(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim();

    if let Ok(parsed) = DateTime::parse_from_rfc2822(trimmed) {
        return Some(parsed.with_timezone(&Utc));
    }

    let normalized = trimmed
        .replace(" GMT", " +0000")
        .replace(" UTC", " +0000")
        .replace(" UT", " +0000");
    if let Ok(parsed) = DateTime::parse_from_rfc2822(&normalized) {
        return Some(parsed.with_timezone(&Utc));
    }

    None
}

fn get_local_built_time_utc() -> Result<DateTime<Utc>> {
    // 1) Prefer container/image injected built time (avoid false positives in Docker)
    // Expected format: RFC3339 (e.g. 2026-01-21T17:02:18+08:00 or 2026-01-21T09:02:18Z)
    if let Ok(value) = std::env::var("BILI_SYNC_IMAGE_BUILT_AT") {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(value.trim()) {
            return Ok(parsed.with_timezone(&Utc));
        }
    }

    // 2) Marker file in Docker image (written by Dockerfile)
    if let Ok(value) = std::fs::read_to_string("/app/image-built-at.txt") {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(value.trim()) {
            return Ok(parsed.with_timezone(&Utc));
        }
    }

    let mut candidates: Vec<DateTime<Utc>> = Vec::new();

    // 3) Rust compile time (built.rs)
    let built_rs_time = DateTime::parse_from_rfc2822(built_info::BUILT_TIME_UTC)
        .context("解析本地构建时间失败（built::BUILT_TIME_UTC）")?
        .with_timezone(&Utc);
    candidates.push(built_rs_time);

    // 4) Executable mtime (some release pipelines preserve build time via mtime)
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(metadata) = std::fs::metadata(exe) {
            if let Ok(modified) = metadata.modified() {
                candidates.push(DateTime::<Utc>::from(modified));
            }
        }
    }

    candidates
        .into_iter()
        .max()
        .ok_or_else(|| anyhow!("无法获取本地构建时间"))
}

async fn fetch_cnb_remote_pushed_at(client: &reqwest::Client, tag: &str) -> Result<DateTime<Utc>> {
    match fetch_cnb_remote_pushed_at_via_registry(client, tag).await {
        Ok(dt) => Ok(dt),
        Err(registry_err) => {
            warn!(error = ?registry_err, tag, "通过 CNB registry 获取推送时间失败，尝试网页兜底");
            match fetch_cnb_remote_pushed_at_via_packages_page(client, tag).await {
                Ok(dt) => Ok(dt),
                Err(page_err) => Err(page_err).context(format!("CNB registry({tag}) 错误: {registry_err:#}")),
            }
        }
    }
}

async fn fetch_cnb_remote_pushed_at_via_packages_page(client: &reqwest::Client, tag: &str) -> Result<DateTime<Utc>> {
    let html = client
        .get(CNB_PACKAGES_PAGE_URL)
        .header(reqwest::header::ACCEPT, "text/html")
        .send()
        .await
        .context("请求 CNB packages 页面失败")?
        .error_for_status()
        .context("CNB packages 页面返回错误状态")?
        .text()
        .await
        .context("读取 CNB packages 页面内容失败")?;

    let next_data_json =
        extract_next_data_json(&html).ok_or_else(|| anyhow!("CNB packages 页面未找到 __NEXT_DATA__"))?;
    let next_data_value: serde_json::Value =
        serde_json::from_str(next_data_json).context("解析 CNB packages __NEXT_DATA__ JSON 失败")?;

    let push_at = find_tag_push_at(&next_data_value, tag)
        .ok_or_else(|| anyhow!("CNB packages __NEXT_DATA__ 中未找到标签 push_at: {tag}"))?;

    let pushed_at = DateTime::parse_from_rfc3339(push_at)
        .with_context(|| format!("解析 push_at 失败: {push_at}"))?
        .with_timezone(&Utc);
    Ok(pushed_at)
}

async fn fetch_cnb_remote_pushed_at_via_registry(client: &reqwest::Client, tag: &str) -> Result<DateTime<Utc>> {
    #[derive(Deserialize)]
    struct TokenResponse {
        token: String,
    }

    let token = client
        .get(CNB_REGISTRY_TOKEN_ENDPOINT)
        .query(&[
            ("service", CNB_REGISTRY_SERVICE),
            ("scope", "repository:sviplk.com/docker/bili-sync:pull"),
        ])
        .send()
        .await
        .context("获取 CNB Registry Token 失败")?
        .error_for_status()
        .context("获取 CNB Registry Token 返回错误状态")?
        .json::<TokenResponse>()
        .await
        .context("解析 CNB Registry Token 响应失败")?
        .token;

    let manifest_url = format!("{CNB_REGISTRY_BASE}/v2/{CNB_BILI_SYNC_REPOSITORY}/manifests/{tag}");
    let manifest_res = client
        .get(&manifest_url)
        .header(
            reqwest::header::ACCEPT,
            [
                "application/vnd.oci.image.index.v1+json",
                "application/vnd.docker.distribution.manifest.list.v2+json",
                "application/vnd.oci.image.manifest.v1+json",
                "application/vnd.docker.distribution.manifest.v2+json",
            ]
            .join(", "),
        )
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("请求 CNB manifest 失败: {manifest_url}"))?
        .error_for_status()
        .with_context(|| format!("CNB manifest 返回错误状态: {manifest_url}"))?;

    if let Some(last_modified) = manifest_res.headers().get(reqwest::header::LAST_MODIFIED) {
        if let Ok(last_modified_str) = last_modified.to_str() {
            if let Some(parsed) = parse_http_date_to_utc(last_modified_str) {
                return Ok(parsed);
            }
        }
    }

    let manifest_value: serde_json::Value = manifest_res.json().await.context("解析 CNB manifest JSON 失败")?;

    async fn fetch_created_time_from_manifest(
        client: &reqwest::Client,
        token: &str,
        manifest_value: &serde_json::Value,
    ) -> Result<DateTime<Utc>> {
        let config_digest = manifest_value
            .get("config")
            .and_then(|v| v.get("digest"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("manifest 中未找到 config.digest"))?;

        let config_url = format!("{CNB_REGISTRY_BASE}/v2/{CNB_BILI_SYNC_REPOSITORY}/blobs/{config_digest}");
        let config_value: serde_json::Value = client
            .get(&config_url)
            .bearer_auth(token)
            .send()
            .await
            .with_context(|| format!("请求 image config 失败: {config_url}"))?
            .error_for_status()
            .with_context(|| format!("image config 返回错误状态: {config_url}"))?
            .json()
            .await
            .context("解析 image config JSON 失败")?;

        let created_str = config_value
            .get("created")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("image config 中未找到 created 字段"))?;

        let created = DateTime::parse_from_rfc3339(created_str)
            .with_context(|| format!("解析 image config.created 失败: {created_str}"))?
            .with_timezone(&Utc);
        Ok(created)
    }

    if let Some(manifests) = manifest_value.get("manifests").and_then(|v| v.as_array()) {
        let selected_digest = manifests
            .iter()
            .find_map(|item| {
                let os = item.get("platform").and_then(|p| p.get("os")).and_then(|v| v.as_str());
                let arch = item
                    .get("platform")
                    .and_then(|p| p.get("architecture"))
                    .and_then(|v| v.as_str());

                if os == Some("linux") && arch == Some("amd64") {
                    item.get("digest").and_then(|v| v.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .or_else(|| {
                manifests
                    .first()
                    .and_then(|item| item.get("digest").and_then(|v| v.as_str()).map(str::to_string))
            })
            .ok_or_else(|| anyhow!("manifest list 中未找到可用 digest"))?;

        let sub_manifest_url = format!("{CNB_REGISTRY_BASE}/v2/{CNB_BILI_SYNC_REPOSITORY}/manifests/{selected_digest}");
        let sub_manifest_value: serde_json::Value = client
            .get(&sub_manifest_url)
            .header(
                reqwest::header::ACCEPT,
                [
                    "application/vnd.oci.image.manifest.v1+json",
                    "application/vnd.docker.distribution.manifest.v2+json",
                ]
                .join(", "),
            )
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| format!("请求子 manifest 失败: {sub_manifest_url}"))?
            .error_for_status()
            .with_context(|| format!("子 manifest 返回错误状态: {sub_manifest_url}"))?
            .json()
            .await
            .context("解析子 manifest JSON 失败")?;

        return fetch_created_time_from_manifest(client, &token, &sub_manifest_value).await;
    }

    fetch_created_time_from_manifest(client, &token, &manifest_value).await
}

/// 标准化文件路径格式
fn normalize_file_path(path: &str) -> String {
    // 将所有反斜杠转换为正斜杠，保持路径一致性
    path.replace('\\', "/")
}

/// 判断路径是否属于“危险删除”范围
///
/// 说明：删除视频源本地文件时，会使用该函数避免误删根目录/盘符等危险路径。
fn is_dangerous_path_for_deletion(path: &str) -> bool {
    let norm = normalize_file_path(path).trim_end_matches('/').to_string();
    if norm.is_empty() || norm == "/" || norm == "\\" {
        return true;
    }

    // Windows 盘符根目录（如 "C:" 或 "F:"）属于高危路径
    #[cfg(windows)]
    {
        let bytes = norm.as_bytes();
        if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return true;
        }
    }

    false
}

/// 删除指定目录（仅当目录存在且为空）
fn cleanup_empty_dir_if_empty(dir: &str, label: &str) {
    use std::fs;
    use std::path::Path;

    let dir_norm = normalize_file_path(dir).trim_end_matches('/').to_string();
    if is_dangerous_path_for_deletion(&dir_norm) {
        return;
    }

    let path = Path::new(&dir_norm);
    if !path.exists() {
        return;
    }

    match fs::read_dir(path) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                match fs::remove_dir(path) {
                    Ok(_) => info!("清理空{}: {}", label, dir_norm),
                    Err(e) => warn!("无法删除空{} {}: {}", label, dir_norm, e),
                }
            }
        }
        Err(e) => warn!("无法读取目录 {}: {}", dir_norm, e),
    }
}

/// 清理空的父目录
///
/// # 参数
/// - `deleted_path`: 已删除的文件夹路径
/// - `stop_at`: 停止清理的父目录路径（避免删除配置的基础路径）
fn cleanup_empty_parent_dirs(deleted_path: &str, stop_at: &str) {
    use std::fs;
    use std::path::Path;

    let stop_at_norm = normalize_file_path(stop_at).trim_end_matches('/').to_string();
    if stop_at_norm.is_empty() {
        return;
    }

    let deleted_path_norm = normalize_file_path(deleted_path).trim_end_matches('/').to_string();
    if deleted_path_norm == stop_at_norm {
        info!("已删除路径等于停止目录，跳过父目录清理: {}", stop_at_norm);
        return;
    }

    let mut current_path = Path::new(&deleted_path_norm).parent();
    while let Some(parent) = current_path {
        let parent_str = parent.to_string_lossy().to_string();
        let parent_norm = normalize_file_path(&parent_str).trim_end_matches('/').to_string();
        if parent_norm == stop_at_norm {
            info!("已达到停止清理目录: {}", parent_norm);
            break;
        }

        // 检查父目录是否为空
        if parent.exists() {
            match fs::read_dir(parent) {
                Ok(mut entries) => {
                    // 如果目录为空（没有子项），则删除它
                    if entries.next().is_none() {
                        match fs::remove_dir(parent) {
                            Ok(_) => {
                                info!("清理空父目录: {}", parent_str);
                                current_path = parent.parent();
                                continue;
                            }
                            Err(e) => {
                                warn!("无法删除空父目录 {}: {}", parent_str, e);
                                break;
                            }
                        }
                    } else {
                        // 目录不为空，停止清理
                        info!("目录不为空，停止清理: {}", parent_str);
                        break;
                    }
                }
                Err(e) => {
                    warn!("无法读取父目录 {}: {}", parent_str, e);
                    break;
                }
            }
        } else {
            break;
        }
    }
}

/// 处理包含路径分隔符的模板结果，对每个路径段单独应用filenamify
/// 这样可以保持目录结构同时确保每个段都是安全的文件名
fn process_path_with_filenamify(input: &str) -> String {
    // 修复：采用与下载流程相同的两阶段处理
    // 阶段1：先对内容进行安全化，保护模板分隔符
    let temp_placeholder = "🔒TEMP_PATH_SEP🔒";
    let protected_input = input.replace("___PATH_SEP___", temp_placeholder);

    // 阶段2：对保护后的内容进行安全化处理（内容中的斜杠会被转换为下划线）
    let safe_content = crate::utils::filenamify::filenamify(&protected_input);

    // 阶段3：恢复模板路径分隔符
    safe_content.replace(temp_placeholder, "/")
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    #[test]
    fn test_process_path_with_filenamify_slash_handling() {
        // 测试与用户报告相同的情况
        let input = "ZHY2020___PATH_SEP___【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身/ドッペルゲンガー」孤独摇滚！总集剧场版Re:Re: OP Lyric MV";
        let result = process_path_with_filenamify(input);

        println!("输入: {}", input);
        println!("输出: {}", result);

        // 验证结果
        assert!(result.starts_with("ZHY2020/"), "应该以 ZHY2020/ 开头");
        assert!(!result.contains("分身/ドッペルゲンガー"), "内容中的斜杠应该被处理");
        assert!(result.contains("分身_ドッペルゲンガー"), "斜杠应该变成下划线");

        // 确保只有一个路径分隔符
        let slash_count = result.matches('/').count();
        assert_eq!(
            slash_count, 1,
            "应该只有一个路径分隔符，但发现了 {}，结果: {}",
            slash_count, result
        );
    }

    #[test]
    fn test_process_path_without_separator() {
        // 测试不包含模板分隔符的情况
        let input = "普通视频标题/带斜杠";
        let result = process_path_with_filenamify(input);

        // 应该将所有斜杠转换为下划线
        assert_eq!(result, "普通视频标题_带斜杠");
        assert!(!result.contains('/'));
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("bili-sync-{}-{}", prefix, uuid::Uuid::new_v4()));
        dir
    }

    #[test]
    fn test_cleanup_empty_parent_dirs_stops_at_stop_at() {
        let root = unique_temp_dir("cleanup-stop-at");
        let base = root.join("base");
        let sub1 = base.join("sub1");
        let sub2 = sub1.join("sub2");

        fs::create_dir_all(&sub2).unwrap();
        fs::remove_dir_all(&sub2).unwrap();

        cleanup_empty_parent_dirs(sub2.to_string_lossy().as_ref(), base.to_string_lossy().as_ref());

        assert!(base.exists(), "不应删除 stop_at 目录");
        assert!(!sub1.exists(), "应清理空的中间父目录");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_cleanup_empty_parent_dirs_deleted_equals_stop_at_noop() {
        let root = unique_temp_dir("cleanup-noop");
        let base = root.join("base");

        fs::create_dir_all(&base).unwrap();

        cleanup_empty_parent_dirs(base.to_string_lossy().as_ref(), base.to_string_lossy().as_ref());

        assert!(base.exists(), "deleted_path 等于 stop_at 时应直接返回");

        let _ = fs::remove_dir_all(&root);
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(get_video_sources, get_videos, get_video, reset_video, reset_all_videos, reset_specific_tasks, update_video_status, add_video_source, update_video_source_enabled, update_video_source_scan_deleted, reset_video_source_path, delete_video_source, reload_config, get_config, update_config, get_bangumi_seasons, search_bilibili, get_user_favorites, get_user_collections, get_user_followings, get_subscribed_collections, get_submission_videos, get_logs, get_queue_status, cancel_queue_task, proxy_image, get_config_item, get_config_history, get_config_migration_status, migrate_config_schema, validate_config, get_hot_reload_status, check_initial_setup, setup_auth_token, update_credential, generate_qr_code, poll_qr_status, get_current_user, clear_credential, pause_scanning_endpoint, resume_scanning_endpoint, get_task_control_status, get_video_play_info, proxy_video_stream, validate_favorite, get_user_favorites_by_uid, test_notification_handler, get_notification_config, update_notification_config, get_notification_status, test_risk_control_handler, get_beta_image_update_status),
    modifiers(&OpenAPIAuth),
    security(
        ("Token" = []),
    )
)]
pub struct ApiDoc;

/// 检查 beta 镜像是否有更新（用于前端角标提示）
#[utoipa::path(
    get,
    path = "/api/updates/beta",
    responses(
        (status = 200, body = ApiResponse<BetaImageUpdateStatusResponse>),
    ),
    security(("Token" = []))
)]
pub async fn get_beta_image_update_status() -> Result<ApiResponse<BetaImageUpdateStatusResponse>, ApiError> {
    let now = Utc::now();

    if let Some((cached_at, cached)) = BETA_IMAGE_UPDATE_CACHE.read().await.clone() {
        if (now - cached_at).num_seconds() < BETA_IMAGE_UPDATE_CACHE_TTL_SECONDS {
            return Ok(ApiResponse::ok(cached));
        }
    }

    let release_channel = get_release_channel();
    let checked_tag = get_checked_tag(&release_channel).to_string();

    let checked_at = crate::utils::time_format::beijing_now().to_rfc3339();
    let beijing_tz = crate::utils::time_format::beijing_timezone();

    let local_built_at = match get_local_built_time_utc() {
        Ok(dt) => Some(dt.with_timezone(&beijing_tz).to_rfc3339()),
        Err(e) => {
            let response = BetaImageUpdateStatusResponse {
                update_available: false,
                release_channel: Some(release_channel),
                checked_tag: Some(checked_tag),
                local_built_at: None,
                remote_pushed_at: None,
                checked_at: Some(checked_at),
                error: Some(format!("{e:#}")),
            };
            *BETA_IMAGE_UPDATE_CACHE.write().await = Some((now, response.clone()));
            return Ok(ApiResponse::ok(response));
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::from(anyhow!("创建 HTTP 客户端失败: {}", e)))?;

    let remote_pushed_at = match fetch_cnb_remote_pushed_at(&client, &checked_tag).await {
        Ok(dt) => Some(dt.with_timezone(&beijing_tz).to_rfc3339()),
        Err(e) => {
            let response = BetaImageUpdateStatusResponse {
                update_available: false,
                release_channel: Some(release_channel),
                checked_tag: Some(checked_tag),
                local_built_at,
                remote_pushed_at: None,
                checked_at: Some(checked_at),
                error: Some(format!("{e:#}")),
            };
            *BETA_IMAGE_UPDATE_CACHE.write().await = Some((now, response.clone()));
            return Ok(ApiResponse::ok(response));
        }
    };

    let update_available = match (&local_built_at, &remote_pushed_at) {
        (Some(local), Some(remote)) => {
            let local = DateTime::parse_from_rfc3339(local).ok().map(|d| d.with_timezone(&Utc));
            let remote = DateTime::parse_from_rfc3339(remote).ok().map(|d| d.with_timezone(&Utc));
            match (local, remote) {
                // 允许一定的时间误差，避免“构建时间/推送时间”相差几分钟导致误判有更新
                (Some(local_dt), Some(remote_dt)) => {
                    let diff_seconds = (remote_dt - local_dt).num_seconds();
                    diff_seconds > 2 * 60
                }
                _ => false,
            }
        }
        _ => false,
    };

    let response = BetaImageUpdateStatusResponse {
        update_available,
        release_channel: Some(release_channel),
        checked_tag: Some(checked_tag),
        local_built_at,
        remote_pushed_at,
        checked_at: Some(checked_at),
        error: None,
    };

    *BETA_IMAGE_UPDATE_CACHE.write().await = Some((now, response.clone()));
    Ok(ApiResponse::ok(response))
}

/// 列出所有视频来源
#[utoipa::path(
    get,
    path = "/api/video-sources",
    responses(
        (status = 200, body = ApiResponse<VideoSourcesResponse>),
    )
)]
pub async fn get_video_sources(
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<VideoSourcesResponse>, ApiError> {
    // 获取各类视频源
    // 使用全模型查询，避免tuple元素数量限制（最多12个）
    let collection_sources: Vec<VideoSource> = collection::Entity::find()
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(|model| {
            let keyword_filters = model
                .keyword_filters
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let blacklist_keywords = model
                .blacklist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let whitelist_keywords = model
                .whitelist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            VideoSource {
                id: model.id,
                name: model.name,
                enabled: model.enabled,
                path: model.path,
                scan_deleted_videos: model.scan_deleted_videos,
                f_id: None,
                s_id: Some(model.s_id),
                m_id: Some(model.m_id),
                collection_type: Some(if model.r#type == 1 { "series" } else { "season" }.to_string()),
                upper_id: None,
                season_id: None,
                media_id: None,
                selected_seasons: None,
                blacklist_keywords,
                whitelist_keywords,
                case_sensitive: model.keyword_case_sensitive,
                keyword_filters,
                keyword_filter_mode: model.keyword_filter_mode,
                audio_only: model.audio_only,
                audio_only_m4a_only: model.audio_only_m4a_only,
                flat_folder: model.flat_folder,
                download_danmaku: model.download_danmaku,
                download_subtitle: model.download_subtitle,
                ai_rename: model.ai_rename,
                ai_rename_video_prompt: model.ai_rename_video_prompt,
                ai_rename_audio_prompt: model.ai_rename_audio_prompt,
                ai_rename_enable_multi_page: model.ai_rename_enable_multi_page,
                ai_rename_enable_collection: model.ai_rename_enable_collection,
                ai_rename_enable_bangumi: model.ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir: model.ai_rename_rename_parent_dir,
                use_dynamic_api: None,
            }
        })
        .collect();

    let favorite_sources: Vec<VideoSource> = favorite::Entity::find()
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(|model| {
            let keyword_filters = model
                .keyword_filters
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let blacklist_keywords = model
                .blacklist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let whitelist_keywords = model
                .whitelist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            VideoSource {
                id: model.id,
                name: model.name,
                enabled: model.enabled,
                path: model.path,
                scan_deleted_videos: model.scan_deleted_videos,
                f_id: Some(model.f_id),
                s_id: None,
                m_id: None,
                collection_type: None,
                upper_id: None,
                season_id: None,
                media_id: None,
                selected_seasons: None,
                blacklist_keywords,
                whitelist_keywords,
                case_sensitive: model.keyword_case_sensitive,
                keyword_filters,
                keyword_filter_mode: model.keyword_filter_mode,
                audio_only: model.audio_only,
                audio_only_m4a_only: model.audio_only_m4a_only,
                flat_folder: model.flat_folder,
                download_danmaku: model.download_danmaku,
                download_subtitle: model.download_subtitle,
                ai_rename: model.ai_rename,
                ai_rename_video_prompt: model.ai_rename_video_prompt,
                ai_rename_audio_prompt: model.ai_rename_audio_prompt,
                ai_rename_enable_multi_page: model.ai_rename_enable_multi_page,
                ai_rename_enable_collection: model.ai_rename_enable_collection,
                ai_rename_enable_bangumi: model.ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir: model.ai_rename_rename_parent_dir,
                use_dynamic_api: None,
            }
        })
        .collect();

    let submission_sources: Vec<VideoSource> = submission::Entity::find()
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(|model| {
            let keyword_filters = model
                .keyword_filters
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let blacklist_keywords = model
                .blacklist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let whitelist_keywords = model
                .whitelist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            VideoSource {
                id: model.id,
                name: model.upper_name.clone(),
                enabled: model.enabled,
                path: model.path,
                scan_deleted_videos: model.scan_deleted_videos,
                f_id: None,
                s_id: None,
                m_id: None,
                collection_type: None,
                upper_id: Some(model.upper_id),
                season_id: None,
                media_id: None,
                selected_seasons: None,
                blacklist_keywords,
                whitelist_keywords,
                case_sensitive: model.keyword_case_sensitive,
                keyword_filters,
                keyword_filter_mode: model.keyword_filter_mode,
                audio_only: model.audio_only,
                audio_only_m4a_only: model.audio_only_m4a_only,
                flat_folder: model.flat_folder,
                download_danmaku: model.download_danmaku,
                download_subtitle: model.download_subtitle,
                ai_rename: model.ai_rename,
                ai_rename_video_prompt: model.ai_rename_video_prompt,
                ai_rename_audio_prompt: model.ai_rename_audio_prompt,
                ai_rename_enable_multi_page: model.ai_rename_enable_multi_page,
                ai_rename_enable_collection: model.ai_rename_enable_collection,
                ai_rename_enable_bangumi: model.ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir: model.ai_rename_rename_parent_dir,
                use_dynamic_api: Some(model.use_dynamic_api),
            }
        })
        .collect();

    let watch_later_sources: Vec<VideoSource> = watch_later::Entity::find()
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(|model| {
            let keyword_filters = model
                .keyword_filters
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let blacklist_keywords = model
                .blacklist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let whitelist_keywords = model
                .whitelist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            VideoSource {
                id: model.id,
                name: "稍后再看".to_string(),
                enabled: model.enabled,
                path: model.path,
                scan_deleted_videos: model.scan_deleted_videos,
                f_id: None,
                s_id: None,
                m_id: None,
                collection_type: None,
                upper_id: None,
                season_id: None,
                media_id: None,
                selected_seasons: None,
                blacklist_keywords,
                whitelist_keywords,
                case_sensitive: model.keyword_case_sensitive,
                keyword_filters,
                keyword_filter_mode: model.keyword_filter_mode,
                audio_only: model.audio_only,
                audio_only_m4a_only: model.audio_only_m4a_only,
                flat_folder: model.flat_folder,
                download_danmaku: model.download_danmaku,
                download_subtitle: model.download_subtitle,
                ai_rename: model.ai_rename,
                ai_rename_video_prompt: model.ai_rename_video_prompt,
                ai_rename_audio_prompt: model.ai_rename_audio_prompt,
                ai_rename_enable_multi_page: model.ai_rename_enable_multi_page,
                ai_rename_enable_collection: model.ai_rename_enable_collection,
                ai_rename_enable_bangumi: model.ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir: model.ai_rename_rename_parent_dir,
                use_dynamic_api: None,
            }
        })
        .collect();

    // 确保bangumi_sources是一个数组，即使为空
    // 由于tuple最多支持12个元素，使用全模型查询方式
    let bangumi_sources: Vec<VideoSource> = video_source::Entity::find()
        .filter(video_source::Column::Type.eq(1))
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(|model| {
            let selected_seasons =
                model
                    .selected_seasons
                    .as_ref()
                    .and_then(|json| match serde_json::from_str::<Vec<String>>(json) {
                        Ok(seasons) if !seasons.is_empty() => Some(seasons),
                        Ok(_) => None,
                        Err(err) => {
                            warn!(
                                "Failed to parse selected_seasons for bangumi source {}: {}",
                                model.id, err
                            );
                            None
                        }
                    });
            let keyword_filters = model
                .keyword_filters
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let blacklist_keywords = model
                .blacklist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
            let whitelist_keywords = model
                .whitelist_keywords
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());

            VideoSource {
                id: model.id,
                name: model.name,
                enabled: model.enabled,
                path: model.path,
                scan_deleted_videos: model.scan_deleted_videos,
                f_id: None,
                s_id: None,
                m_id: None,
                collection_type: None,
                upper_id: None,
                season_id: model.season_id,
                media_id: model.media_id,
                selected_seasons,
                blacklist_keywords,
                whitelist_keywords,
                case_sensitive: model.keyword_case_sensitive,
                keyword_filters,
                keyword_filter_mode: model.keyword_filter_mode,
                audio_only: model.audio_only,
                audio_only_m4a_only: model.audio_only_m4a_only,
                flat_folder: model.flat_folder,
                download_danmaku: model.download_danmaku,
                download_subtitle: model.download_subtitle,
                ai_rename: model.ai_rename,
                ai_rename_video_prompt: model.ai_rename_video_prompt,
                ai_rename_audio_prompt: model.ai_rename_audio_prompt,
                ai_rename_enable_multi_page: model.ai_rename_enable_multi_page,
                ai_rename_enable_collection: model.ai_rename_enable_collection,
                ai_rename_enable_bangumi: model.ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir: model.ai_rename_rename_parent_dir,
                use_dynamic_api: None,
            }
        })
        .collect();

    // 返回响应，确保每个分类都是一个数组
    Ok(ApiResponse::ok(VideoSourcesResponse {
        collection: collection_sources,
        favorite: favorite_sources,
        submission: submission_sources,
        watch_later: watch_later_sources,
        bangumi: bangumi_sources,
    }))
}

fn resolution_to_height_range(resolution: u32) -> Option<(u32, u32)> {
    match resolution {
        // 说明：B站视频存在“非标准高度”的情况（例如 1920x1078），
        // 仅按固定区间会把它误判为 720P。这里按相邻档位的“中点”划分区间，
        // 让非标准高度更接近其应归属的档位。
        //
        // 档位：2160/1440/1080/720/480/360
        // 分界：1800/1260/900/600/420
        2160 => Some((1800, 99999)),
        1440 => Some((1260, 1799)),
        1080 => Some((900, 1259)),
        720 => Some((600, 899)),
        480 => Some((420, 599)),
        360 => Some((0, 419)),
        _ => None,
    }
}

fn resolve_height_filters_parts(
    min_height: Option<u32>,
    max_height: Option<u32>,
    resolution: Option<u32>,
) -> (Option<u32>, Option<u32>) {
    if min_height.is_some() || max_height.is_some() {
        return (min_height, max_height);
    }

    if let Some(resolution_value) = resolution {
        if let Some((min, max)) = resolution_to_height_range(resolution_value) {
            return (Some(min), Some(max));
        }
    }

    (None, None)
}

fn resolve_height_filters(params: &VideosRequest) -> (Option<u32>, Option<u32>) {
    resolve_height_filters_parts(params.min_height, params.max_height, params.resolution)
}

/// 列出视频的基本信息，支持根据视频来源筛选、名称查找和分页
#[utoipa::path(
    get,
    path = "/api/videos",
    params(
        VideosRequest,
    ),
    responses(
        (status = 200, body = ApiResponse<VideosResponse>),
    )
)]
pub async fn get_videos(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Query(params): Query<VideosRequest>,
) -> Result<ApiResponse<VideosResponse>, ApiError> {
    let mut query = video::Entity::find();
    let (min_height, max_height) = resolve_height_filters(&params);

    // 根据配置决定是否过滤已删除的视频
    let scan_deleted = crate::config::with_config(|bundle| bundle.config.scan_deleted_videos);
    if !scan_deleted {
        query = query.filter(video::Column::Deleted.eq(0));
    }

    // 直接检查是否存在bangumi参数，单独处理
    if let Some(id) = params.bangumi {
        query = query.filter(video::Column::SourceId.eq(id).and(video::Column::SourceType.eq(1)));
    } else {
        // 处理其他常规类型
        for (field, column) in [
            (params.collection, video::Column::CollectionId),
            (params.favorite, video::Column::FavoriteId),
            (params.submission, video::Column::SubmissionId),
            (params.watch_later, video::Column::WatchLaterId),
        ] {
            if let Some(id) = field {
                query = query.filter(column.eq(id));
            }
        }
    }
    if let Some(query_word) = params.query {
        query = query.filter(
            video::Column::Name
                .contains(&query_word)
                .or(video::Column::Path.contains(&query_word)),
        );
    }

    // 筛选失败任务（仅显示下载状态中包含失败的视频）
    if params.show_failed_only.unwrap_or(false) {
        // download_status是u32类型，使用位运算编码5个子任务状态
        // 每3位表示一个子任务：(download_status >> (offset * 3)) & 7
        // 状态值：0=未开始，1-6=失败次数，7=成功
        // 筛选任一子任务状态在1-6范围内的视频
        use sea_orm::sea_query::Expr;

        let mut conditions = Vec::new();

        // 检查5个子任务位置的状态
        for offset in 0..5 {
            let shift = offset * 3;
            // 提取第offset个子任务状态: (download_status >> shift) & 7
            // 检查是否为失败状态: >= 1 AND <= 6
            conditions.push(Expr::cust(format!(
                "((download_status >> {}) & 7) BETWEEN 1 AND 6",
                shift
            )));
        }

        // 使用OR连接：任一子任务失败即匹配
        let mut final_condition = conditions[0].clone();
        for condition in conditions.into_iter().skip(1) {
            final_condition = final_condition.or(condition);
        }

        query = query.filter(final_condition);
    }

    if min_height.is_some() || max_height.is_some() {
        let mut page_query = page::Entity::find().select_only().column(page::Column::VideoId);

        if let Some(min_height_value) = min_height {
            page_query = page_query.filter(page::Column::Height.gte(min_height_value));
        }
        if let Some(max_height_value) = max_height {
            page_query = page_query.filter(page::Column::Height.lte(max_height_value));
        }

        let video_ids: Vec<i32> = page_query
            .group_by(page::Column::VideoId)
            .into_tuple::<i32>()
            .all(db.as_ref())
            .await?;

        if video_ids.is_empty() {
            return Ok(ApiResponse::ok(VideosResponse {
                videos: Vec::new(),
                total_count: 0,
            }));
        }

        query = query.filter(video::Column::Id.is_in(video_ids));
    }

    let total_count = query.clone().count(db.as_ref()).await?;
    let (page, page_size) = if let (Some(page), Some(page_size)) = (params.page, params.page_size) {
        (page, page_size)
    } else {
        (1, 10)
    };

    // 处理排序参数
    let sort_by = params.sort_by.as_deref().unwrap_or("id");
    let sort_order = params.sort_order.as_deref().unwrap_or("desc");

    // 应用排序
    query = match sort_by {
        "name" => {
            if sort_order == "asc" {
                query.order_by_asc(video::Column::Name)
            } else {
                query.order_by_desc(video::Column::Name)
            }
        }
        "upper_name" => {
            if sort_order == "asc" {
                query.order_by_asc(video::Column::UpperName)
            } else {
                query.order_by_desc(video::Column::UpperName)
            }
        }
        "created_at" => {
            // 添加时间（入库时间）
            if sort_order == "asc" {
                query.order_by_asc(video::Column::CreatedAt)
            } else {
                query.order_by_desc(video::Column::CreatedAt)
            }
        }
        "pubtime" => {
            // 发布时间（视频在B站的发布时间）
            if sort_order == "asc" {
                query.order_by_asc(video::Column::Pubtime)
            } else {
                query.order_by_desc(video::Column::Pubtime)
            }
        }
        _ => {
            // 默认按ID排序
            if sort_order == "asc" {
                query.order_by_asc(video::Column::Id)
            } else {
                query.order_by_desc(video::Column::Id)
            }
        }
    };

    Ok(ApiResponse::ok(VideosResponse {
        videos: {
            // 查询包含season_id和source_type字段，用于番剧标题获取
            type RawVideoTuple = (
                i32,
                String,
                String,
                String,
                String,
                i32,
                u32,
                String,
                bool,
                Option<String>,
                Option<i32>,
            );
            let raw_videos: Vec<RawVideoTuple> = query
                .select_only()
                .columns([
                    video::Column::Id,
                    video::Column::Bvid,
                    video::Column::Name,
                    video::Column::UpperName,
                    video::Column::Path,
                    video::Column::Category,
                    video::Column::DownloadStatus,
                    video::Column::Cover,
                    video::Column::Valid,
                    video::Column::SeasonId,
                    video::Column::SourceType,
                ])
                .into_tuple::<(
                    i32,
                    String,
                    String,
                    String,
                    String,
                    i32,
                    u32,
                    String,
                    bool,
                    Option<String>,
                    Option<i32>,
                )>()
                .paginate(db.as_ref(), page_size)
                .fetch_page(page)
                .await?;

            // 转换为VideoInfo并填充番剧标题
            let mut videos: Vec<VideoInfo> = raw_videos
                .iter()
                .map(
                    |(
                        id,
                        bvid,
                        name,
                        upper_name,
                        path,
                        category,
                        download_status,
                        cover,
                        valid,
                        _season_id,
                        _source_type,
                    )| {
                        VideoInfo::from((
                            *id,
                            bvid.clone(),
                            name.clone(),
                            upper_name.clone(),
                            path.clone(),
                            *category,
                            *download_status,
                            cover.clone(),
                            *valid,
                        ))
                    },
                )
                .collect();

            // 为番剧类型的视频填充真实标题
            for (
                i,
                (
                    _id,
                    _bvid,
                    _name,
                    _upper_name,
                    _path,
                    _category,
                    _download_status,
                    _cover,
                    _valid,
                    season_id,
                    source_type,
                ),
            ) in raw_videos.iter().enumerate()
            {
                if *source_type == Some(1) && season_id.is_some() {
                    // 番剧类型且有season_id，尝试获取真实标题
                    if let Some(ref season_id_str) = season_id {
                        // 先从缓存获取
                        if let Some(title) = get_cached_season_title(season_id_str).await {
                            videos[i].bangumi_title = Some(title);
                        } else {
                            // 缓存中没有，尝试从API获取并存入缓存
                            if let Some(title) = fetch_and_cache_season_title(season_id_str).await {
                                videos[i].bangumi_title = Some(title);
                            }
                        }
                    }
                }
            }

            videos
        },
        total_count,
    }))
}

/// 获取视频详细信息，包括关联的所有 page
#[utoipa::path(
    get,
    path = "/api/videos/{id}",
    responses(
        (status = 200, body = ApiResponse<VideoResponse>),
    )
)]
pub async fn get_video(
    Path(id): Path<i32>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<VideoResponse>, ApiError> {
    let raw_video = video::Entity::find_by_id(id)
        .select_only()
        .columns([
            video::Column::Id,
            video::Column::Bvid,
            video::Column::Name,
            video::Column::UpperName,
            video::Column::Path,
            video::Column::Category,
            video::Column::DownloadStatus,
            video::Column::Cover,
            video::Column::Valid,
            video::Column::SeasonId,
            video::Column::SourceType,
        ])
        .into_tuple::<(
            i32,
            String,
            String,
            String,
            String,
            i32,
            u32,
            String,
            bool,
            Option<String>,
            Option<i32>,
        )>()
        .one(db.as_ref())
        .await?;

    let Some((_id, bvid, name, upper_name, path, category, download_status, cover, valid, season_id, source_type)) =
        raw_video
    else {
        return Err(InnerApiError::NotFound(id).into());
    };

    // 创建VideoInfo并填充bangumi_title
    let mut video_info = VideoInfo::from((
        _id,
        bvid,
        name,
        upper_name,
        path,
        category,
        download_status,
        cover,
        valid,
    ));

    // 为番剧类型的视频填充真实标题
    if source_type == Some(1) && season_id.is_some() {
        // 番剧类型且有season_id，尝试获取真实标题
        if let Some(ref season_id_str) = season_id {
            // 先从缓存获取
            if let Some(title) = get_cached_season_title(season_id_str).await {
                video_info.bangumi_title = Some(title);
            } else {
                // 缓存中没有，尝试从API获取并存入缓存
                if let Some(title) = fetch_and_cache_season_title(season_id_str).await {
                    video_info.bangumi_title = Some(title);
                }
            }
        }
    }
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(id))
        .order_by_asc(page::Column::Pid)
        .select_only()
        .columns([
            page::Column::Id,
            page::Column::Pid,
            page::Column::Name,
            page::Column::DownloadStatus,
            page::Column::Path,
        ])
        .into_tuple::<(i32, i32, String, u32, Option<String>)>()
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(PageInfo::from)
        .collect();
    Ok(ApiResponse::ok(VideoResponse {
        video: video_info,
        pages,
    }))
}

/// 重置视频的下载状态
#[utoipa::path(
    post,
    path = "/api/videos/{id}/reset",
    params(
        ("id" = i32, Path, description = "Video ID"),
        ("force" = Option<bool>, Query, description = "Force reset all tasks including successful ones")
    ),
    responses(
        (status = 200, body = ApiResponse<ResetVideoResponse>),
    )
)]
pub async fn reset_video(
    Path(id): Path<i32>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<ResetVideoResponse>, ApiError> {
    // 检查是否强制重置
    let force_reset = params
        .get("force")
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);

    // 获取视频和分页信息
    let (video_info, pages_info) = tokio::try_join!(
        video::Entity::find_by_id(id)
            .select_only()
            .columns([
                video::Column::Id,
                video::Column::Bvid,
                video::Column::Name,
                video::Column::UpperName,
                video::Column::Path,
                video::Column::Category,
                video::Column::DownloadStatus,
                video::Column::Cover,
                video::Column::Valid,
            ])
            .into_tuple::<(i32, String, String, String, String, i32, u32, String, bool)>()
            .one(db.as_ref()),
        page::Entity::find()
            .filter(page::Column::VideoId.eq(id))
            .order_by_asc(page::Column::Pid)
            .select_only()
            .columns([
                page::Column::Id,
                page::Column::Pid,
                page::Column::Name,
                page::Column::DownloadStatus,
            ])
            .into_tuple::<(i32, i32, String, u32)>()
            .all(db.as_ref())
    )?;

    let Some(video_info) = video_info else {
        return Err(InnerApiError::NotFound(id).into());
    };

    let mut video_info = VideoInfo::from(video_info);
    let resetted_pages_info = pages_info
        .into_iter()
        .filter_map(|(page_id, pid, name, download_status)| {
            let mut page_status = PageStatus::from(download_status);
            let should_reset = if force_reset {
                page_status.reset_all()
            } else {
                page_status.reset_failed()
            };
            if should_reset {
                Some(PageInfo::from((page_id, pid, name, page_status.into())))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let mut video_status = VideoStatus::from(video_info.download_status);
    let mut video_resetted = if force_reset {
        video_status.reset_all()
    } else {
        video_status.reset_failed()
    };

    if !resetted_pages_info.is_empty() {
        video_status.set(4, 0); // 将"分P下载"重置为 0
        video_resetted = true;
    }

    if video_resetted {
        video_info.download_status = video_status.into();
    }

    let resetted = video_resetted || !resetted_pages_info.is_empty();

    if resetted {
        let txn = db.begin().await?;

        if video_resetted {
            video::Entity::update(video::ActiveModel {
                id: Unchanged(id),
                download_status: Set(VideoStatus::from(video_info.download_status).into()),
                valid: Set(true),
                ..Default::default()
            })
            .exec(&txn)
            .await?;
        }

        if !resetted_pages_info.is_empty() {
            for page in &resetted_pages_info {
                page::Entity::update(page::ActiveModel {
                    id: Unchanged(page.id),
                    download_status: Set(PageStatus::from(page.download_status).into()),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        txn.commit().await?;
    }

    // 获取所有分页信息（包括未重置的）
    let all_pages_info = page::Entity::find()
        .filter(page::Column::VideoId.eq(id))
        .order_by_asc(page::Column::Pid)
        .select_only()
        .columns([
            page::Column::Id,
            page::Column::Pid,
            page::Column::Name,
            page::Column::DownloadStatus,
        ])
        .into_tuple::<(i32, i32, String, u32)>()
        .all(db.as_ref())
        .await?
        .into_iter()
        .map(PageInfo::from)
        .collect();

    Ok(ApiResponse::ok(ResetVideoResponse {
        resetted,
        video: video_info,
        pages: all_pages_info,
    }))
}

/// 重置所有视频和页面的失败状态为未下载状态，这样在下次下载任务中会触发重试
#[utoipa::path(
    post,
    path = "/api/videos/reset-all",
    params(
        ("collection" = Option<i32>, Query, description = "合集ID"),
        ("favorite" = Option<i32>, Query, description = "收藏夹ID"),
        ("submission" = Option<i32>, Query, description = "UP主投稿ID"),
        ("bangumi" = Option<i32>, Query, description = "番剧ID"),
        ("watch_later" = Option<i32>, Query, description = "稍后观看ID"),
    ),
    responses(
        (status = 200, body = ApiResponse<ResetAllVideosResponse>),
    )
)]
pub async fn reset_all_videos(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Query(params): Query<crate::api::request::VideosRequest>,
) -> Result<ApiResponse<ResetAllVideosResponse>, ApiError> {
    use std::collections::HashSet;

    // 构建查询条件，与get_videos保持一致（但不使用分页）
    let mut video_query = video::Entity::find();
    let (min_height, max_height) = resolve_height_filters(&params);

    // 根据配置决定是否过滤已删除的视频
    let scan_deleted = crate::config::with_config(|bundle| bundle.config.scan_deleted_videos);
    if !scan_deleted {
        video_query = video_query.filter(video::Column::Deleted.eq(0));
    }

    // 直接检查是否存在bangumi参数，单独处理
    if let Some(id) = params.bangumi {
        video_query = video_query.filter(video::Column::SourceId.eq(id).and(video::Column::SourceType.eq(1)));
    } else {
        // 处理其他常规类型
        for (field, column) in [
            (params.collection, video::Column::CollectionId),
            (params.favorite, video::Column::FavoriteId),
            (params.submission, video::Column::SubmissionId),
            (params.watch_later, video::Column::WatchLaterId),
        ] {
            if let Some(id) = field {
                video_query = video_query.filter(column.eq(id));
            }
        }
    }

    if let Some(query_word) = params.query.as_ref() {
        video_query = video_query.filter(
            video::Column::Name
                .contains(query_word)
                .or(video::Column::Path.contains(query_word)),
        );
    }

    // 筛选失败任务（仅显示下载状态中包含失败的视频）
    if params.show_failed_only.unwrap_or(false) {
        use sea_orm::sea_query::Expr;

        let mut conditions = Vec::new();
        for offset in 0..5 {
            let shift = offset * 3;
            conditions.push(Expr::cust(format!(
                "((download_status >> {}) & 7) BETWEEN 1 AND 6",
                shift
            )));
        }

        let mut final_condition = conditions[0].clone();
        for condition in conditions.into_iter().skip(1) {
            final_condition = final_condition.or(condition);
        }
        video_query = video_query.filter(final_condition);
    }

    // 分辨率筛选：通过 page.height 反查 video_id，再与 video_query 求交集
    if min_height.is_some() || max_height.is_some() {
        let mut page_query = page::Entity::find().select_only().column(page::Column::VideoId);

        if let Some(min_height_value) = min_height {
            page_query = page_query.filter(page::Column::Height.gte(min_height_value));
        }
        if let Some(max_height_value) = max_height {
            page_query = page_query.filter(page::Column::Height.lte(max_height_value));
        }

        let video_ids: Vec<i32> = page_query
            .group_by(page::Column::VideoId)
            .into_tuple::<i32>()
            .all(db.as_ref())
            .await?;

        if video_ids.is_empty() {
            return Ok(ApiResponse::ok(ResetAllVideosResponse {
                resetted: false,
                resetted_videos_count: 0,
                resetted_pages_count: 0,
            }));
        }

        video_query = video_query.filter(video::Column::Id.is_in(video_ids));
    }

    // 先查询符合条件的视频（不分页）
    let all_videos = video_query
        .select_only()
        .columns([
            video::Column::Id,
            video::Column::Bvid,
            video::Column::Name,
            video::Column::UpperName,
            video::Column::Path,
            video::Column::Category,
            video::Column::DownloadStatus,
            video::Column::Cover,
            video::Column::Valid,
        ])
        .into_tuple::<(i32, String, String, String, String, i32, u32, String, bool)>()
        .all(db.as_ref())
        .await?;

    if all_videos.is_empty() {
        return Ok(ApiResponse::ok(ResetAllVideosResponse {
            resetted: false,
            resetted_videos_count: 0,
            resetted_pages_count: 0,
        }));
    }

    let selected_video_ids: Vec<i32> = all_videos.iter().map(|(id, ..)| *id).collect();

    // 获取选中视频的所有分页信息（不再额外限制 height：与列表页行为一致，按 video 维度筛选）
    let all_pages = page::Entity::find()
        .filter(page::Column::VideoId.is_in(selected_video_ids))
        .select_only()
        .columns([
            page::Column::Id,
            page::Column::Pid,
            page::Column::Name,
            page::Column::DownloadStatus,
            page::Column::VideoId,
        ])
        .into_tuple::<(i32, i32, String, u32, i32)>()
        .all(db.as_ref())
        .await?;

    // 获取force参数，默认为false
    let force_reset = params.force.unwrap_or(false);

    // 处理页面重置
    let resetted_pages_info = all_pages
        .into_iter()
        .filter_map(|(id, pid, name, download_status, video_id)| {
            let mut page_status = PageStatus::from(download_status);
            let should_reset = if force_reset {
                page_status.reset_all()
            } else {
                page_status.reset_failed()
            };
            if should_reset {
                let page_info = PageInfo::from((id, pid, name, page_status.into()));
                Some((page_info, video_id))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let video_ids_with_resetted_pages: HashSet<i32> =
        resetted_pages_info.iter().map(|(_, video_id)| *video_id).collect();

    let resetted_pages_info: Vec<PageInfo> = resetted_pages_info
        .into_iter()
        .map(|(page_info, _)| page_info)
        .collect();

    let all_videos_info: Vec<VideoInfo> = all_videos.into_iter().map(VideoInfo::from).collect();

    let resetted_videos_info = all_videos_info
        .into_iter()
        .filter_map(|mut video_info| {
            let mut video_status = VideoStatus::from(video_info.download_status);
            let mut video_resetted = if force_reset {
                video_status.reset_all()
            } else {
                video_status.reset_failed()
            };
            if video_ids_with_resetted_pages.contains(&video_info.id) {
                video_status.set(4, 0); // 将"分P下载"重置为 0
                video_resetted = true;
            }
            if video_resetted {
                video_info.download_status = video_status.into();
                Some(video_info)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let resetted = !(resetted_videos_info.is_empty() && resetted_pages_info.is_empty());

    if resetted {
        let txn = db.begin().await?;

        // 批量更新视频状态 + 开启自动下载
        if !resetted_videos_info.is_empty() {
            for video in &resetted_videos_info {
                video::Entity::update(video::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(video.id),
                    download_status: sea_orm::Set(VideoStatus::from(video.download_status).into()),
                    auto_download: sea_orm::Set(true),
                    valid: sea_orm::Set(true),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        // 批量更新页面状态
        if !resetted_pages_info.is_empty() {
            for page in &resetted_pages_info {
                page::Entity::update(page::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(page.id),
                    download_status: sea_orm::Set(PageStatus::from(page.download_status).into()),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        txn.commit().await?;

        // 开启这些视频的自动下载，避免被过滤（与 scan 流程对齐）
        if !resetted_videos_info.is_empty() {
            for video in &resetted_videos_info {
                video::Entity::update(video::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(video.id),
                    auto_download: sea_orm::Set(true),
                    ..Default::default()
                })
                .exec(db.as_ref())
                .await?;
            }
        }
    }

    // 触发立即扫描（缩短等待）
    crate::task::resume_scanning();
    // 触发立即扫描（缩短等待）
    if resetted {
        crate::task::resume_scanning();
    }
    Ok(ApiResponse::ok(ResetAllVideosResponse {
        resetted,
        resetted_videos_count: resetted_videos_info.len(),
        resetted_pages_count: resetted_pages_info.len(),
    }))
}

/// 强制重置特定任务状态（不管当前状态）
#[utoipa::path(
    post,
    path = "/api/videos/reset-specific-tasks",
    request_body = ResetSpecificTasksRequest,
    responses(
        (status = 200, body = ApiResponse<ResetAllVideosResponse>),
    )
)]
pub async fn reset_specific_tasks(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(request): axum::Json<crate::api::request::ResetSpecificTasksRequest>,
) -> Result<ApiResponse<ResetAllVideosResponse>, ApiError> {
    use std::collections::HashSet;

    let task_indexes = &request.task_indexes;
    if task_indexes.is_empty() {
        return Err(crate::api::error::InnerApiError::BadRequest("至少需要选择一个任务".to_string()).into());
    }

    // 验证任务索引范围
    for &index in task_indexes {
        if index > 4 {
            return Err(crate::api::error::InnerApiError::BadRequest(format!("无效的任务索引: {}", index)).into());
        }
    }

    // 构建查询条件，与get_videos保持一致
    let mut video_query = video::Entity::find();
    let (min_height, max_height) =
        resolve_height_filters_parts(request.min_height, request.max_height, request.resolution);

    // 根据配置决定是否过滤已删除的视频
    let scan_deleted = crate::config::with_config(|bundle| bundle.config.scan_deleted_videos);
    if !scan_deleted {
        video_query = video_query.filter(video::Column::Deleted.eq(0));
    }

    // 直接检查是否存在bangumi参数，单独处理
    if let Some(id) = request.bangumi {
        video_query = video_query.filter(video::Column::SourceId.eq(id).and(video::Column::SourceType.eq(1)));
    } else {
        // 处理其他常规类型
        for (field, column) in [
            (request.collection, video::Column::CollectionId),
            (request.favorite, video::Column::FavoriteId),
            (request.submission, video::Column::SubmissionId),
            (request.watch_later, video::Column::WatchLaterId),
        ] {
            if let Some(id) = field {
                video_query = video_query.filter(column.eq(id));
            }
        }
    }

    if let Some(query_word) = request.query.as_ref() {
        video_query = video_query.filter(
            video::Column::Name
                .contains(query_word)
                .or(video::Column::Path.contains(query_word)),
        );
    }

    // 筛选失败任务（仅显示下载状态中包含失败的视频）
    if request.show_failed_only.unwrap_or(false) {
        use sea_orm::sea_query::Expr;

        let mut conditions = Vec::new();
        for offset in 0..5 {
            let shift = offset * 3;
            conditions.push(Expr::cust(format!(
                "((download_status >> {}) & 7) BETWEEN 1 AND 6",
                shift
            )));
        }

        let mut final_condition = conditions[0].clone();
        for condition in conditions.into_iter().skip(1) {
            final_condition = final_condition.or(condition);
        }
        video_query = video_query.filter(final_condition);
    }

    // 分辨率筛选：通过 page.height 反查 video_id，再与 video_query 求交集
    if min_height.is_some() || max_height.is_some() {
        let mut page_query = page::Entity::find().select_only().column(page::Column::VideoId);

        if let Some(min_height_value) = min_height {
            page_query = page_query.filter(page::Column::Height.gte(min_height_value));
        }
        if let Some(max_height_value) = max_height {
            page_query = page_query.filter(page::Column::Height.lte(max_height_value));
        }

        let video_ids: Vec<i32> = page_query
            .group_by(page::Column::VideoId)
            .into_tuple::<i32>()
            .all(db.as_ref())
            .await?;

        if video_ids.is_empty() {
            return Ok(ApiResponse::ok(ResetAllVideosResponse {
                resetted: false,
                resetted_videos_count: 0,
                resetted_pages_count: 0,
            }));
        }

        video_query = video_query.filter(video::Column::Id.is_in(video_ids));
    }

    // 查询符合条件的视频（不分页）
    let all_videos = video_query
        .select_only()
        .columns([
            video::Column::Id,
            video::Column::Bvid,
            video::Column::Name,
            video::Column::UpperName,
            video::Column::Path,
            video::Column::Category,
            video::Column::DownloadStatus,
            video::Column::Cover,
            video::Column::Valid,
            video::Column::CollectionId,
            video::Column::SinglePage,
        ])
        .into_tuple::<(
            i32,
            String,
            String,
            String,
            String,
            i32,
            u32,
            String,
            bool,
            Option<i32>,
            Option<bool>,
        )>()
        .all(db.as_ref())
        .await?;

    if all_videos.is_empty() {
        return Ok(ApiResponse::ok(ResetAllVideosResponse {
            resetted: false,
            resetted_videos_count: 0,
            resetted_pages_count: 0,
        }));
    }

    let selected_video_ids: Vec<i32> = all_videos.iter().map(|(id, ..)| *id).collect();

    // 获取选中视频的所有分页信息（按 video 维度筛选）
    let all_pages = page::Entity::find()
        .filter(page::Column::VideoId.is_in(selected_video_ids))
        .select_only()
        .columns([
            page::Column::Id,
            page::Column::Pid,
            page::Column::Name,
            page::Column::DownloadStatus,
            page::Column::VideoId,
        ])
        .into_tuple::<(i32, i32, String, u32, i32)>()
        .all(db.as_ref())
        .await?;

    let force_reset = request.force.unwrap_or(false);

    // 处理页面重置 - 强制重置指定任务（不管当前状态）
    let resetted_pages_info = all_pages
        .into_iter()
        .filter_map(|(id, pid, name, download_status, video_id)| {
            let mut page_status = PageStatus::from(download_status);
            let mut page_resetted = false;

            // 重置指定的任务索引：默认仅重置失败任务；force=true 时重置所有非 0 状态
            for &task_index in task_indexes {
                if task_index < 5 {
                    let current_status = page_status.get(task_index);
                    let should_reset = if force_reset {
                        current_status != 0
                    } else {
                        (1..=6).contains(&current_status)
                    };
                    if should_reset {
                        page_status.set(task_index, 0); // 重置为未开始
                        page_resetted = true;
                    }
                }
            }

            if page_resetted {
                let page_info = PageInfo::from((id, pid, name, page_status.into()));
                Some((page_info, video_id))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let video_ids_with_resetted_pages: HashSet<i32> =
        resetted_pages_info.iter().map(|(_, video_id)| *video_id).collect();

    let resetted_pages_info: Vec<PageInfo> = resetted_pages_info
        .into_iter()
        .map(|(page_info, _)| page_info)
        .collect();

    let all_videos_info: Vec<VideoInfo> = all_videos
        .iter()
        .cloned()
        .map(
            |(id, bvid, name, upper_name, path, category, download_status, cover, valid, _, _)| {
                VideoInfo::from((
                    id,
                    bvid,
                    name,
                    upper_name,
                    path,
                    category,
                    download_status,
                    cover,
                    valid,
                ))
            },
        )
        .collect();

    let resetted_videos_info = all_videos_info
        .into_iter()
        .filter_map(|mut video_info| {
            let mut video_status = VideoStatus::from(video_info.download_status);
            let mut video_resetted = false;

            // 重置指定任务：默认仅重置失败任务；force=true 时重置所有非 0 状态
            for &task_index in task_indexes {
                if task_index < 5 {
                    let current_status = video_status.get(task_index);
                    let should_reset = if force_reset {
                        current_status != 0
                    } else {
                        (1..=6).contains(&current_status)
                    };
                    if should_reset {
                        video_status.set(task_index, 0); // 重置为未开始
                        video_resetted = true;
                    }
                }
            }

            // 如果有分页被重置，同时重置分P下载状态
            if video_ids_with_resetted_pages.contains(&video_info.id) {
                video_status.set(4, 0); // 将"分P下载"重置为 0
                video_resetted = true;
            }

            if video_resetted {
                video_info.download_status = video_status.into();
                Some(video_info)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let resetted = !(resetted_videos_info.is_empty() && resetted_pages_info.is_empty());

    if resetted {
        let txn = db.begin().await?;

        // 批量更新视频状态
        if !resetted_videos_info.is_empty() {
            for video in &resetted_videos_info {
                video::Entity::update(video::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(video.id),
                    download_status: sea_orm::Set(VideoStatus::from(video.download_status).into()),
                    valid: sea_orm::Set(true),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        // 批量更新页面状态
        if !resetted_pages_info.is_empty() {
            for page in &resetted_pages_info {
                page::Entity::update(page::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(page.id),
                    download_status: sea_orm::Set(PageStatus::from(page.download_status).into()),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        txn.commit().await?;
    }

    // 重置视频封面时，同步删除根目录 poster.jpg / folder.jpg，
    // 以便下次执行封面任务时可以重新下载（否则会被“存在即跳过”优化拦截）。
    if task_indexes.contains(&0) {
        use tokio::fs;

        let config = crate::config::reload_config();
        let mut series_roots: HashSet<String> = HashSet::new();

        for (_, _, _, _, path, category, _, _, _, collection_id, single_page) in &all_videos {
            if path.is_empty() {
                continue;
            }

            let is_bangumi = *category == 1;
            let is_collection = collection_id.is_some();
            let is_multi_page = matches!(single_page, Some(false));

            let should_have_root_posters = is_bangumi
                || (is_collection && config.collection_use_season_structure)
                || (is_multi_page && config.multi_page_use_season_structure);

            if should_have_root_posters {
                series_roots.insert(path.clone());
            }
        }

        let mut deleted_count = 0usize;
        for root in series_roots {
            let root = std::path::PathBuf::from(root);
            for file_name in ["poster.jpg", "folder.jpg"] {
                let file_path = root.join(file_name);
                match fs::metadata(&file_path).await {
                    Ok(meta) if meta.is_file() => match fs::remove_file(&file_path).await {
                        Ok(_) => {
                            deleted_count += 1;
                            debug!("已删除根目录封面文件: {:?}", file_path);
                        }
                        Err(e) => warn!("删除根目录封面文件失败: {:?} - {}", file_path, e),
                    },
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        }

        if deleted_count > 0 {
            info!(
                "重置视频封面：已清理 {} 个根目录封面文件（poster.jpg/folder.jpg）",
                deleted_count
            );
        }
    }

    Ok(ApiResponse::ok(ResetAllVideosResponse {
        resetted,
        resetted_videos_count: resetted_videos_info.len(),
        resetted_pages_count: resetted_pages_info.len(),
    }))
}

/// 测试风控验证（开发调试用）
#[utoipa::path(
    post,
    path = "/api/test/risk-control",
    responses(
        (status = 200, description = "测试风控验证结果", body = ApiResponse<crate::api::response::TestRiskControlResponse>),
        (status = 400, description = "配置错误", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn test_risk_control_handler() -> Result<ApiResponse<crate::api::response::TestRiskControlResponse>, ApiError>
{
    use crate::config::with_config;

    tracing::info!("开始测试风控验证功能");

    // 获取风控配置
    let risk_config = with_config(|bundle| bundle.config.risk_control.clone());

    if !risk_config.enabled {
        return Ok(ApiResponse::bad_request(
            crate::api::response::TestRiskControlResponse {
                success: false,
                message: "风控验证功能未启用，请在设置中启用后重试".to_string(),
                verification_url: None,
                instructions: Some("请前往设置页面的'验证码风控'部分启用风控验证功能".to_string()),
            },
        ));
    }

    match risk_config.mode.as_str() {
        "skip" => Ok(ApiResponse::ok(crate::api::response::TestRiskControlResponse {
            success: true,
            message: "风控模式设置为跳过，测试完成".to_string(),
            verification_url: None,
            instructions: Some("当前风控模式为'跳过'，实际使用时将直接跳过验证".to_string()),
        })),
        "manual" => Ok(ApiResponse::ok(crate::api::response::TestRiskControlResponse {
            success: true,
            message: "手动验证模式配置正确，可以处理风控验证".to_string(),
            verification_url: Some("/captcha".to_string()),
            instructions: Some(format!(
                "当前配置为手动验证模式。\n\
                     超时时间: {} 秒\n\
                     当遇到真实风控时，验证界面将在 /captcha 页面显示",
                risk_config.timeout
            )),
        })),
        "auto" => {
            let auto_config = risk_config.auto_solve.as_ref();
            if auto_config.is_none() {
                return Ok(ApiResponse::bad_request(
                    crate::api::response::TestRiskControlResponse {
                        success: false,
                        message: "自动验证模式需要配置验证码识别服务".to_string(),
                        verification_url: None,
                        instructions: Some("请在设置中配置验证码识别服务的API密钥".to_string()),
                    },
                ));
            }

            let auto_config = auto_config.unwrap();
            Ok(ApiResponse::ok(crate::api::response::TestRiskControlResponse {
                success: true,
                message: format!(
                    "自动验证模式配置正确。配置的服务: {}，最大重试次数: {}",
                    auto_config.service, auto_config.max_retries
                ),
                verification_url: None,
                instructions: Some(format!(
                    "当前配置的自动验证服务: {}\n\
                     API密钥: {}...\n\
                     最大重试次数: {}\n\
                     单次超时时间: {} 秒\n\
                     实际使用时将自动调用验证码识别服务完成验证",
                    auto_config.service,
                    if auto_config.api_key.len() > 8 {
                        &auto_config.api_key[..8]
                    } else {
                        "未配置"
                    },
                    auto_config.max_retries,
                    auto_config.solve_timeout
                )),
            }))
        }
        _ => Ok(ApiResponse::bad_request(
            crate::api::response::TestRiskControlResponse {
                success: false,
                message: format!("无效的风控模式: {}", risk_config.mode),
                verification_url: None,
                instructions: Some("请设置有效的风控模式: manual、auto 或 skip".to_string()),
            },
        )),
    }
}

/// 更新特定视频及其所含分页的状态位
#[utoipa::path(
    post,
    path = "/api/videos/{id}/update-status",
    request_body = UpdateVideoStatusRequest,
    responses(
        (status = 200, body = ApiResponse<UpdateVideoStatusResponse>),
    )
)]
pub async fn update_video_status(
    Path(id): Path<i32>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(request): axum::Json<UpdateVideoStatusRequest>,
) -> Result<ApiResponse<UpdateVideoStatusResponse>, ApiError> {
    let (video_info, pages_info) = tokio::try_join!(
        video::Entity::find_by_id(id)
            .select_only()
            .columns([
                video::Column::Id,
                video::Column::Bvid,
                video::Column::Name,
                video::Column::UpperName,
                video::Column::Path,
                video::Column::Category,
                video::Column::DownloadStatus,
                video::Column::Cover,
                video::Column::Valid,
            ])
            .into_tuple::<(i32, String, String, String, String, i32, u32, String, bool)>()
            .one(db.as_ref()),
        page::Entity::find()
            .filter(page::Column::VideoId.eq(id))
            .order_by_asc(page::Column::Cid)
            .select_only()
            .columns([
                page::Column::Id,
                page::Column::Pid,
                page::Column::Name,
                page::Column::DownloadStatus,
            ])
            .into_tuple::<(i32, i32, String, u32)>()
            .all(db.as_ref())
    )?;

    let Some(video_info) = video_info else {
        return Err(InnerApiError::NotFound(id).into());
    };

    let mut video_info = VideoInfo::from(video_info);
    let mut video_status = VideoStatus::from(video_info.download_status);

    // 应用视频状态更新
    for update in &request.video_updates {
        if update.status_index < 5 {
            video_status.set(update.status_index, update.status_value);
        }
    }
    video_info.download_status = video_status.into();

    let mut pages_info: Vec<PageInfo> = pages_info.into_iter().map(PageInfo::from).collect();

    let mut updated_pages_info = Vec::new();
    let mut page_id_map = pages_info
        .iter_mut()
        .map(|page| (page.id, page))
        .collect::<std::collections::HashMap<_, _>>();

    // 应用页面状态更新
    for page_update in &request.page_updates {
        if let Some(page_info) = page_id_map.remove(&page_update.page_id) {
            let mut page_status = PageStatus::from(page_info.download_status);
            for update in &page_update.updates {
                if update.status_index < 5 {
                    page_status.set(update.status_index, update.status_value);
                }
            }
            page_info.download_status = page_status.into();
            updated_pages_info.push(page_info);
        }
    }

    let has_video_updates = !request.video_updates.is_empty();
    let has_page_updates = !updated_pages_info.is_empty();

    if has_video_updates || has_page_updates {
        let txn = db.begin().await?;

        if has_video_updates {
            video::Entity::update(video::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(video_info.id),
                download_status: sea_orm::Set(VideoStatus::from(video_info.download_status).into()),
                auto_download: sea_orm::Set(true),
                valid: sea_orm::Set(true),
                ..Default::default()
            })
            .exec(&txn)
            .await?;
        }

        if has_page_updates {
            for page in &updated_pages_info {
                page::Entity::update(page::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(page.id),
                    download_status: sea_orm::Set(PageStatus::from(page.download_status).into()),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        txn.commit().await?;
    }

    // 触发立即扫描（缩短等待）
    if has_video_updates || has_page_updates {
        crate::task::resume_scanning();
    }
    Ok(ApiResponse::ok(UpdateVideoStatusResponse {
        success: has_video_updates || has_page_updates,
        video: video_info,
        pages: pages_info,
    }))
}

/// 获取现有番剧源列表（用于合并选择）
#[utoipa::path(
    get,
    path = "/api/video-sources/bangumi/list",
    responses(
        (status = 200, body = ApiResponse<BangumiSourceListResponse>),
    )
)]
pub async fn get_bangumi_sources_for_merge(
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<BangumiSourceListResponse>, ApiError> {
    // 获取所有番剧源
    let bangumi_sources = video_source::Entity::find()
        .filter(video_source::Column::Type.eq(1)) // 番剧类型
        .filter(video_source::Column::Enabled.eq(true)) // 只返回启用的番剧
        .order_by_desc(video_source::Column::CreatedAt)
        .all(db.as_ref())
        .await?;

    let mut bangumi_options = Vec::new();

    for source in bangumi_sources {
        // 计算选中的季度数量
        let selected_seasons_count = if source.download_all_seasons.unwrap_or(false) {
            0 // 全部季度模式不计算具体数量
        } else if let Some(ref seasons_json) = source.selected_seasons {
            serde_json::from_str::<Vec<String>>(seasons_json)
                .map(|seasons| seasons.len())
                .unwrap_or(0)
        } else {
            0
        };

        bangumi_options.push(BangumiSourceOption {
            id: source.id,
            name: source.name,
            path: source.path,
            season_id: source.season_id,
            media_id: source.media_id,
            download_all_seasons: source.download_all_seasons.unwrap_or(false),
            selected_seasons_count,
        });
    }

    let total_count = bangumi_options.len();

    Ok(ApiResponse::ok(BangumiSourceListResponse {
        success: true,
        bangumi_sources: bangumi_options,
        total_count,
    }))
}

/// 添加新的视频源
#[utoipa::path(
    post,
    path = "/api/video-sources",
    request_body = AddVideoSourceRequest,
    responses(
        (status = 200, body = ApiResponse<AddVideoSourceResponse>),
    )
)]
pub async fn add_video_source(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(params): axum::Json<AddVideoSourceRequest>,
) -> Result<ApiResponse<AddVideoSourceResponse>, ApiError> {
    // 检查是否正在扫描
    if crate::task::is_scanning() {
        // 正在扫描，将添加任务加入队列
        let task_id = uuid::Uuid::new_v4().to_string();
        let add_task = crate::task::AddVideoSourceTask {
            source_type: params.source_type.clone(),
            name: params.name.clone(),
            source_id: params.source_id.clone(),
            path: params.path.clone(),
            up_id: params.up_id.clone(),
            collection_type: params.collection_type.clone(),
            media_id: params.media_id.clone(),
            ep_id: params.ep_id.clone(),
            download_all_seasons: params.download_all_seasons,
            selected_seasons: params.selected_seasons.clone(),
            task_id: task_id.clone(),
        };

        crate::task::enqueue_add_task(add_task, &db).await?;

        info!(
            "检测到正在扫描，添加任务已加入队列等待处理: {} 名称={}",
            params.source_type, params.name
        );

        return Ok(ApiResponse::ok(AddVideoSourceResponse {
            success: true,
            source_id: 0, // 队列中的任务还没有ID
            source_type: params.source_type,
            message: "正在扫描中，添加任务已加入队列，将在扫描完成后自动处理".to_string(),
        }));
    }

    // 没有扫描，直接执行添加
    match add_video_source_internal(db, params).await {
        Ok(response) => Ok(ApiResponse::ok(response)),
        Err(e) => Err(e),
    }
}

/// 内部添加视频源函数（用于队列处理和直接调用）
pub async fn add_video_source_internal(
    db: Arc<DatabaseConnection>,
    params: AddVideoSourceRequest,
) -> Result<AddVideoSourceResponse, ApiError> {
    // 使用主数据库连接

    let txn = db.begin().await?;

    let result = match params.source_type.as_str() {
        "collection" => {
            // 验证合集必需的参数
            let up_id_str = params
                .up_id
                .as_ref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("合集类型需要提供UP主ID"))?;

            let up_id = up_id_str.parse::<i64>().map_err(|_| anyhow!("无效的UP主ID"))?;
            let s_id = params.source_id.parse::<i64>().map_err(|_| anyhow!("无效的合集ID"))?;

            let collection_type_value = params.collection_type.as_deref().unwrap_or("season");
            let collection_type = match collection_type_value {
                "season" => 2, // 视频合集
                "series" => 1, // 视频列表
                _ => 2,        // 默认使用season类型
            };

            // 检查是否已存在相同的合集（按 sid + mid + type 唯一）
            let existing_collection = collection::Entity::find()
                .filter(collection::Column::SId.eq(s_id))
                .filter(collection::Column::MId.eq(up_id))
                .filter(collection::Column::Type.eq(collection_type))
                .one(&txn)
                .await?;

            if let Some(existing) = existing_collection {
                return Err(anyhow!(
                    "合集已存在！类型：{}，合集名称：\"{}\"，合集ID：{}，UP主ID：{}，保存路径：{}。如需修改设置，请先删除现有合集再重新添加。",
                    if existing.r#type == 1 { "series" } else { "season" },
                    existing.name,
                    existing.s_id,
                    existing.m_id,
                    existing.path
                ).into());
            }

            let collection_name = params.name.clone();

            // 调试日志：显示前端传递的cover参数
            match &params.cover {
                Some(cover) => info!("前端传递的cover参数: \"{}\"", cover),
                None => info!("前端未传递cover参数"),
            }

            // 如果前端没有传递封面URL，尝试从API获取
            let cover_url = match &params.cover {
                Some(cover) if !cover.is_empty() => {
                    info!("使用前端提供的封面URL: {}", cover);
                    params.cover.clone()
                }
                _ => {
                    // 前端没有传递封面，尝试从API获取
                    info!("前端未提供封面URL，尝试从API获取合集「{}」的封面", collection_name);
                    // 创建BiliClient实例
                    let config = crate::config::reload_config();
                    let credential = config.credential.load();
                    let cookie = credential
                        .as_ref()
                        .map(|cred| {
                            format!(
                                "SESSDATA={};bili_jct={};buvid3={};DedeUserID={};ac_time_value={}",
                                cred.sessdata, cred.bili_jct, cred.buvid3, cred.dedeuserid, cred.ac_time_value
                            )
                        })
                        .unwrap_or_default();
                    let client = crate::bilibili::BiliClient::new(cookie);
                    match get_collection_cover_from_api(up_id, s_id, &client).await {
                        Ok(cover) => {
                            info!("成功从API获取合集「{}」封面: {}", collection_name, cover);
                            Some(cover)
                        }
                        Err(e) => {
                            warn!("从API获取合集「{}」封面失败: {}", collection_name, e);
                            None
                        }
                    }
                }
            };

            // 处理关键词过滤器
            let keyword_filters_json = params
                .keyword_filters
                .as_ref()
                .filter(|kf| !kf.is_empty())
                .map(|kf| serde_json::to_string(kf).unwrap_or_default());

            // 处理关键词过滤模式
            let keyword_filter_mode = params.keyword_filter_mode.clone();

            let collection = collection::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                s_id: sea_orm::Set(s_id),
                m_id: sea_orm::Set(up_id),
                name: sea_orm::Set(params.name),
                r#type: sea_orm::Set(collection_type),
                path: sea_orm::Set(params.path.clone()),
                created_at: sea_orm::Set(now_standard_string()),
                latest_row_at: sea_orm::Set("1970-01-01 00:00:00".to_string()),
                enabled: sea_orm::Set(true),
                scan_deleted_videos: sea_orm::Set(false),
                cover: sea_orm::Set(cover_url),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode),
                blacklist_keywords: sea_orm::Set(None),
                whitelist_keywords: sea_orm::Set(None),
                keyword_case_sensitive: sea_orm::Set(true),
                audio_only: sea_orm::Set(params.audio_only.unwrap_or(false)),
                audio_only_m4a_only: sea_orm::Set(params.audio_only_m4a_only.unwrap_or(false)),
                flat_folder: sea_orm::Set(params.flat_folder.unwrap_or(false)),
                download_danmaku: sea_orm::Set(params.download_danmaku.unwrap_or(true)),
                download_subtitle: sea_orm::Set(params.download_subtitle.unwrap_or(true)),
                ai_rename: sea_orm::Set(params.ai_rename.unwrap_or(false)),
                ai_rename_video_prompt: sea_orm::Set(params.ai_rename_video_prompt.clone().unwrap_or_default()),
                ai_rename_audio_prompt: sea_orm::Set(params.ai_rename_audio_prompt.clone().unwrap_or_default()),
                ai_rename_enable_multi_page: sea_orm::Set(params.ai_rename_enable_multi_page.unwrap_or(false)),
                ai_rename_enable_collection: sea_orm::Set(params.ai_rename_enable_collection.unwrap_or(false)),
                ai_rename_enable_bangumi: sea_orm::Set(params.ai_rename_enable_bangumi.unwrap_or(false)),
                ai_rename_rename_parent_dir: sea_orm::Set(params.ai_rename_rename_parent_dir.unwrap_or(false)),
            };

            let insert_result = collection::Entity::insert(collection).exec(&txn).await?;

            info!("合集添加成功: {} (ID: {}, UP主: {})", collection_name, s_id, up_id);

            AddVideoSourceResponse {
                success: true,
                source_id: insert_result.last_insert_id,
                source_type: "collection".to_string(),
                message: "合集添加成功".to_string(),
            }
        }
        "favorite" => {
            let f_id = params.source_id.parse::<i64>().map_err(|_| anyhow!("无效的收藏夹ID"))?;

            // 检查是否已存在相同的收藏夹
            let existing_favorite = favorite::Entity::find()
                .filter(favorite::Column::FId.eq(f_id))
                .one(&txn)
                .await?;

            if let Some(existing) = existing_favorite {
                return Err(anyhow!(
                    "收藏夹已存在！收藏夹名称：\"{}\"，收藏夹ID：{}，保存路径：{}。如需修改设置，请先删除现有收藏夹再重新添加。",
                    existing.name,
                    existing.f_id,
                    existing.path
                ).into());
            }

            // 添加收藏夹
            let favorite_name = params.name.clone();

            // 处理关键词过滤器
            let keyword_filters_json = params
                .keyword_filters
                .as_ref()
                .filter(|kf| !kf.is_empty())
                .map(|kf| serde_json::to_string(kf).unwrap_or_default());

            // 处理关键词过滤模式
            let keyword_filter_mode = params.keyword_filter_mode.clone();

            let favorite = favorite::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                f_id: sea_orm::Set(f_id),
                name: sea_orm::Set(params.name),
                path: sea_orm::Set(params.path.clone()),
                created_at: sea_orm::Set(now_standard_string()),
                latest_row_at: sea_orm::Set("1970-01-01 00:00:00".to_string()),
                enabled: sea_orm::Set(true),
                scan_deleted_videos: sea_orm::Set(false),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode),
                blacklist_keywords: sea_orm::Set(None),
                whitelist_keywords: sea_orm::Set(None),
                keyword_case_sensitive: sea_orm::Set(true),
                audio_only: sea_orm::Set(params.audio_only.unwrap_or(false)),
                audio_only_m4a_only: sea_orm::Set(params.audio_only_m4a_only.unwrap_or(false)),
                flat_folder: sea_orm::Set(params.flat_folder.unwrap_or(false)),
                download_danmaku: sea_orm::Set(params.download_danmaku.unwrap_or(true)),
                download_subtitle: sea_orm::Set(params.download_subtitle.unwrap_or(true)),
                ai_rename: sea_orm::Set(params.ai_rename.unwrap_or(false)),
                ai_rename_video_prompt: sea_orm::Set(params.ai_rename_video_prompt.clone().unwrap_or_default()),
                ai_rename_audio_prompt: sea_orm::Set(params.ai_rename_audio_prompt.clone().unwrap_or_default()),
                ai_rename_enable_multi_page: sea_orm::Set(params.ai_rename_enable_multi_page.unwrap_or(false)),
                ai_rename_enable_collection: sea_orm::Set(params.ai_rename_enable_collection.unwrap_or(false)),
                ai_rename_enable_bangumi: sea_orm::Set(params.ai_rename_enable_bangumi.unwrap_or(false)),
                ai_rename_rename_parent_dir: sea_orm::Set(params.ai_rename_rename_parent_dir.unwrap_or(false)),
            };

            let insert_result = favorite::Entity::insert(favorite).exec(&txn).await?;

            info!("收藏夹添加成功: {} (ID: {})", favorite_name, f_id);

            AddVideoSourceResponse {
                success: true,
                source_id: insert_result.last_insert_id,
                source_type: "favorite".to_string(),
                message: "收藏夹添加成功".to_string(),
            }
        }
        "submission" => {
            let upper_id = params.source_id.parse::<i64>().map_err(|_| anyhow!("无效的UP主ID"))?;

            // 检查是否已存在相同的UP主投稿
            let existing_submission = submission::Entity::find()
                .filter(submission::Column::UpperId.eq(upper_id))
                .one(&txn)
                .await?;

            if let Some(existing) = existing_submission {
                return Err(anyhow!(
                    "UP主投稿已存在！UP主名称：\"{}\"，UP主ID：{}，保存路径：{}。如需修改设置，请先删除现有UP主投稿再重新添加。",
                    existing.upper_name,
                    existing.upper_id,
                    existing.path
                ).into());
            }

            // 添加UP主投稿
            let upper_name = params.name.clone();

            // 处理关键词过滤器
            let keyword_filters_json = params
                .keyword_filters
                .as_ref()
                .filter(|kf| !kf.is_empty())
                .map(|kf| serde_json::to_string(kf).unwrap_or_default());

            // 处理关键词过滤模式
            let keyword_filter_mode = params.keyword_filter_mode.clone();

            let submission = submission::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                upper_id: sea_orm::Set(upper_id),
                upper_name: sea_orm::Set(params.name),
                path: sea_orm::Set(params.path.clone()),
                created_at: sea_orm::Set(now_standard_string()),
                latest_row_at: sea_orm::Set("1970-01-01 00:00:00".to_string()),
                enabled: sea_orm::Set(true),
                scan_deleted_videos: sea_orm::Set(false),
                last_scan_at: sea_orm::Set(None),
                next_scan_at: sea_orm::Set(None),
                no_update_streak: sea_orm::Set(0),
                selected_videos: sea_orm::Set(
                    params
                        .selected_videos
                        .map(|videos| serde_json::to_string(&videos).unwrap_or_default()),
                ),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode),
                blacklist_keywords: sea_orm::Set(None),
                whitelist_keywords: sea_orm::Set(None),
                keyword_case_sensitive: sea_orm::Set(true),
                audio_only: sea_orm::Set(params.audio_only.unwrap_or(false)),
                download_danmaku: sea_orm::Set(params.download_danmaku.unwrap_or(true)),
                download_subtitle: sea_orm::Set(params.download_subtitle.unwrap_or(true)),
                ai_rename: sea_orm::Set(params.ai_rename.unwrap_or(false)),
                ai_rename_video_prompt: sea_orm::Set(params.ai_rename_video_prompt.clone().unwrap_or_default()),
                ai_rename_audio_prompt: sea_orm::Set(params.ai_rename_audio_prompt.clone().unwrap_or_default()),
                ai_rename_enable_multi_page: sea_orm::Set(params.ai_rename_enable_multi_page.unwrap_or(false)),
                ai_rename_enable_collection: sea_orm::Set(params.ai_rename_enable_collection.unwrap_or(false)),
                ai_rename_enable_bangumi: sea_orm::Set(params.ai_rename_enable_bangumi.unwrap_or(false)),
                ai_rename_rename_parent_dir: sea_orm::Set(params.ai_rename_rename_parent_dir.unwrap_or(false)),
                audio_only_m4a_only: sea_orm::Set(params.audio_only_m4a_only.unwrap_or(false)),
                flat_folder: sea_orm::Set(params.flat_folder.unwrap_or(false)),
                use_dynamic_api: sea_orm::Set(params.use_dynamic_api.unwrap_or(false)),
                dynamic_api_full_synced: sea_orm::Set(params.use_dynamic_api.unwrap_or(false)),
            };

            let insert_result = submission::Entity::insert(submission).exec(&txn).await?;

            info!("UP主投稿添加成功: {} (ID: {})", upper_name, upper_id);

            AddVideoSourceResponse {
                success: true,
                source_id: insert_result.last_insert_id,
                source_type: "submission".to_string(),
                message: "UP主投稿添加成功".to_string(),
            }
        }
        "bangumi" => {
            // 验证至少有一个ID不为空
            if params.source_id.is_empty() && params.media_id.is_none() && params.ep_id.is_none() {
                return Err(anyhow!("番剧标识不能全部为空，请至少提供 season_id、media_id 或 ep_id 中的一个").into());
            }

            // 如果指定了合并目标，进行合并操作并提交事务
            if let Some(merge_target_id) = params.merge_to_source_id {
                let result = handle_bangumi_merge_to_existing(&txn, params, merge_target_id).await?;
                txn.commit().await?;
                return Ok(result);
            }

            // 检查是否已存在相同的番剧（Season ID完全匹配）
            let existing_query = video_source::Entity::find().filter(video_source::Column::Type.eq(1)); // 番剧类型

            // 1. 首先检查 Season ID 是否重复（精确匹配）
            let mut existing_bangumi = None;

            if !params.source_id.is_empty() {
                // 如果有 season_id，检查是否已存在该 season_id
                existing_bangumi = existing_query
                    .clone()
                    .filter(video_source::Column::SeasonId.eq(&params.source_id))
                    .one(&txn)
                    .await?;
            }

            if existing_bangumi.is_none() {
                if let Some(ref media_id) = params.media_id {
                    // 如果只有 media_id，检查是否已存在该 media_id
                    existing_bangumi = existing_query
                        .clone()
                        .filter(video_source::Column::MediaId.eq(media_id))
                        .one(&txn)
                        .await?;
                } else if let Some(ref ep_id) = params.ep_id {
                    // 如果只有 ep_id，检查是否已存在该 ep_id
                    existing_bangumi = existing_query
                        .clone()
                        .filter(video_source::Column::EpId.eq(ep_id))
                        .one(&txn)
                        .await?;
                }
            }

            if let Some(mut existing) = existing_bangumi {
                // 情况1：Season ID 重复 → 合并到现有番剧源
                info!("检测到重复番剧 Season ID，执行智能合并: {}", existing.name);

                let download_all_seasons = params.download_all_seasons.unwrap_or(false);
                let mut updated = false;
                let mut merge_message = String::new();

                // 如果新请求要下载全部季度，直接更新现有配置
                if download_all_seasons {
                    if !existing.download_all_seasons.unwrap_or(false) {
                        existing.download_all_seasons = Some(true);
                        existing.selected_seasons = None; // 清空特定季度选择
                        updated = true;
                        merge_message = "已更新为下载全部季度".to_string();
                    } else {
                        merge_message = "已配置为下载全部季度，无需更改".to_string();
                    }
                } else {
                    // 处理特定季度的合并
                    if let Some(new_seasons) = params.selected_seasons {
                        if !new_seasons.is_empty() {
                            let mut current_seasons: Vec<String> = Vec::new();

                            // 获取现有的季度选择
                            if let Some(ref seasons_json) = existing.selected_seasons {
                                if let Ok(seasons) = serde_json::from_str::<Vec<String>>(seasons_json) {
                                    current_seasons = seasons;
                                }
                            }

                            // 合并新的季度（去重）
                            let mut all_seasons = current_seasons.clone();
                            let mut added_seasons = Vec::new();

                            for season in new_seasons {
                                if !all_seasons.contains(&season) {
                                    all_seasons.push(season.clone());
                                    added_seasons.push(season);
                                }
                            }

                            if !added_seasons.is_empty() {
                                // 有新季度需要添加
                                let seasons_json = serde_json::to_string(&all_seasons)?;
                                existing.selected_seasons = Some(seasons_json);
                                existing.download_all_seasons = Some(false); // 确保不是全部下载模式
                                updated = true;

                                merge_message = if added_seasons.len() == 1 {
                                    format!("已添加新季度: {}", added_seasons.join(", "))
                                } else {
                                    format!("已添加 {} 个新季度: {}", added_seasons.len(), added_seasons.join(", "))
                                };
                            } else {
                                // 所有季度都已存在
                                merge_message = "所选季度已存在于现有配置中，无需更改".to_string();
                            }
                        }
                    }
                }

                // 更新保存路径（如果提供了不同的路径）
                if !params.path.is_empty() && params.path != existing.path {
                    existing.path = params.path.clone();
                    updated = true;

                    if !merge_message.is_empty() {
                        merge_message.push('，');
                    }
                    merge_message.push_str(&format!("保存路径已更新为: {}", params.path));
                }

                // 更新番剧名称（如果提供了不同的名称）
                if !params.name.is_empty() && params.name != existing.name {
                    existing.name = params.name.clone();
                    updated = true;

                    if !merge_message.is_empty() {
                        merge_message.push('，');
                    }
                    merge_message.push_str(&format!("番剧名称已更新为: {}", params.name));
                }

                if updated {
                    // 更新数据库记录 - 修复：正确使用ActiveModel更新
                    let mut existing_update = video_source::ActiveModel {
                        id: sea_orm::ActiveValue::Unchanged(existing.id),
                        latest_row_at: sea_orm::Set(crate::utils::time_format::now_standard_string()),
                        ..Default::default()
                    };

                    // 根据实际修改的字段设置对应的ActiveModel字段
                    if download_all_seasons && !existing.download_all_seasons.unwrap_or(false) {
                        // 切换到下载全部季度模式
                        existing_update.download_all_seasons = sea_orm::Set(Some(true));
                        existing_update.selected_seasons = sea_orm::Set(None); // 清空特定季度选择
                    } else if !download_all_seasons {
                        // 处理特定季度的合并或更新
                        if let Some(ref new_seasons_json) = existing.selected_seasons {
                            existing_update.selected_seasons = sea_orm::Set(Some(new_seasons_json.clone()));
                            existing_update.download_all_seasons = sea_orm::Set(Some(false));
                        }
                    }

                    // 更新路径（如果有变更）
                    if !params.path.is_empty() && params.path != existing.path {
                        existing_update.path = sea_orm::Set(params.path.clone());
                    }

                    // 更新名称（如果有变更）
                    if !params.name.is_empty() && params.name != existing.name {
                        existing_update.name = sea_orm::Set(params.name.clone());
                    }

                    video_source::Entity::update(existing_update).exec(&txn).await?;

                    // 确保目标路径存在
                    std::fs::create_dir_all(&existing.path).map_err(|e| anyhow!("创建目录失败: {}", e))?;

                    info!("番剧配置合并成功: {}", merge_message);

                    AddVideoSourceResponse {
                        success: true,
                        source_id: existing.id,
                        source_type: "bangumi".to_string(),
                        message: format!("番剧配置已成功合并！{}", merge_message),
                    }
                } else {
                    // 没有实际更新
                    AddVideoSourceResponse {
                        success: true,
                        source_id: existing.id,
                        source_type: "bangumi".to_string(),
                        message: format!("番剧已存在，{}", merge_message),
                    }
                }
            } else {
                // 情况2：Season ID 不重复，检查季度重复并跳过
                let download_all_seasons = params.download_all_seasons.unwrap_or(false);
                let mut final_selected_seasons = params.selected_seasons.clone();
                let mut skipped_seasons = Vec::new();

                // 如果不是下载全部季度，且指定了特定季度，则检查季度重复
                if !download_all_seasons {
                    if let Some(ref new_seasons) = params.selected_seasons {
                        if !new_seasons.is_empty() {
                            // 获取所有现有番剧源的已选季度
                            let all_existing_sources = video_source::Entity::find()
                                .filter(video_source::Column::Type.eq(1))
                                .all(&txn)
                                .await?;

                            let mut all_existing_seasons = std::collections::HashSet::new();

                            for source in all_existing_sources {
                                // 如果该番剧源配置为下载全部季度，我们无法确定具体季度，跳过检查
                                if source.download_all_seasons.unwrap_or(false) {
                                    continue;
                                }

                                // 获取该番剧源的已选季度
                                if let Some(ref seasons_json) = source.selected_seasons {
                                    if let Ok(seasons) = serde_json::from_str::<Vec<String>>(seasons_json) {
                                        for season in seasons {
                                            all_existing_seasons.insert(season);
                                        }
                                    }
                                }
                            }

                            // 过滤掉重复的季度
                            let mut unique_seasons = Vec::new();
                            for season in new_seasons {
                                if all_existing_seasons.contains(season) {
                                    skipped_seasons.push(season.clone());
                                } else {
                                    unique_seasons.push(season.clone());
                                }
                            }

                            final_selected_seasons = Some(unique_seasons);
                        }
                    }
                }

                // 如果所有季度都被跳过了，返回错误
                // 但是如果用户没有提供任何选择的季度，我们允许通过（用于单季度番剧的情况）
                if !download_all_seasons && final_selected_seasons.as_ref().is_none_or(|s| s.is_empty()) {
                    // 只有当用户明确选择了季度但这些季度都被跳过时才报错
                    // 如果用户根本没有选择任何季度，我们允许通过（处理单季度番剧）
                    if !skipped_seasons.is_empty() {
                        let skipped_msg =
                            format!("所选季度已在其他番剧源中存在，已跳过: {}", skipped_seasons.join(", "));
                        return Err(anyhow!(
                            "无法添加番剧：{}。请选择其他季度或使用'下载全部季度'选项。",
                            skipped_msg
                        )
                        .into());
                    }
                    // 如果没有跳过的季度且没有选择的季度，说明是单季度番剧，允许通过
                }

                // 处理选中的季度
                let selected_seasons_json = if !download_all_seasons && final_selected_seasons.is_some() {
                    let seasons = final_selected_seasons.clone().unwrap();
                    if seasons.is_empty() {
                        None
                    } else {
                        Some(serde_json::to_string(&seasons)?)
                    }
                } else {
                    None
                };

                // 处理关键词过滤器
                let keyword_filters_json = params
                    .keyword_filters
                    .as_ref()
                    .filter(|kf| !kf.is_empty())
                    .map(|kf| serde_json::to_string(kf).unwrap_or_default());

                // 处理关键词过滤模式
                let keyword_filter_mode = params.keyword_filter_mode.clone();

                let bangumi = video_source::ActiveModel {
                    id: sea_orm::ActiveValue::NotSet,
                    name: sea_orm::Set(params.name),
                    path: sea_orm::Set(params.path.clone()),
                    r#type: sea_orm::Set(1), // 1表示番剧类型
                    latest_row_at: sea_orm::Set(crate::utils::time_format::now_standard_string()),
                    created_at: sea_orm::Set(crate::utils::time_format::now_standard_string()),
                    season_id: sea_orm::Set(Some(params.source_id.clone())),
                    media_id: sea_orm::Set(params.media_id),
                    ep_id: sea_orm::Set(params.ep_id),
                    download_all_seasons: sea_orm::Set(Some(download_all_seasons)),
                    selected_seasons: sea_orm::Set(selected_seasons_json),
                    keyword_filters: sea_orm::Set(keyword_filters_json),
                    keyword_filter_mode: sea_orm::Set(keyword_filter_mode),
                    audio_only: sea_orm::Set(params.audio_only.unwrap_or(false)),
                    download_danmaku: sea_orm::Set(params.download_danmaku.unwrap_or(true)),
                    download_subtitle: sea_orm::Set(params.download_subtitle.unwrap_or(true)),
                    ai_rename: sea_orm::Set(params.ai_rename.unwrap_or(false)),
                    ai_rename_video_prompt: sea_orm::Set(params.ai_rename_video_prompt.clone().unwrap_or_default()),
                    ai_rename_audio_prompt: sea_orm::Set(params.ai_rename_audio_prompt.clone().unwrap_or_default()),
                    ai_rename_enable_multi_page: sea_orm::Set(params.ai_rename_enable_multi_page.unwrap_or(false)),
                    ai_rename_enable_collection: sea_orm::Set(params.ai_rename_enable_collection.unwrap_or(false)),
                    ai_rename_enable_bangumi: sea_orm::Set(params.ai_rename_enable_bangumi.unwrap_or(false)),
                    ai_rename_rename_parent_dir: sea_orm::Set(params.ai_rename_rename_parent_dir.unwrap_or(false)),
                    ..Default::default()
                };

                let insert_result = video_source::Entity::insert(bangumi).exec(&txn).await?;

                // 确保目标路径存在
                std::fs::create_dir_all(&params.path).map_err(|e| anyhow!("创建目录失败: {}", e))?;

                let success_message = if !skipped_seasons.is_empty() {
                    format!(
                        "番剧添加成功！已跳过重复季度: {}，添加的季度: {}",
                        skipped_seasons.join(", "),
                        final_selected_seasons.unwrap_or_default().join(", ")
                    )
                } else {
                    "番剧添加成功".to_string()
                };

                info!("新番剧添加完成: {}", success_message);

                AddVideoSourceResponse {
                    success: true,
                    source_id: insert_result.last_insert_id,
                    source_type: "bangumi".to_string(),
                    message: success_message,
                }
            }
        }
        "watch_later" => {
            // 稍后观看只能有一个，检查是否已存在
            let existing = watch_later::Entity::find().count(&txn).await?;

            if existing > 0 {
                // 获取现有的稍后观看配置信息
                let existing_watch_later = watch_later::Entity::find()
                    .one(&txn)
                    .await?
                    .ok_or_else(|| anyhow!("数据库状态异常"))?;

                return Err(anyhow!(
                    "稍后观看已存在！保存路径：{}。一个系统只能配置一个稍后观看源，如需修改路径，请先删除现有配置再重新添加。",
                    existing_watch_later.path
                ).into());
            }

            // 处理关键词过滤器
            let keyword_filters_json = params
                .keyword_filters
                .as_ref()
                .filter(|kf| !kf.is_empty())
                .map(|kf| serde_json::to_string(kf).unwrap_or_default());

            // 处理关键词过滤模式
            let keyword_filter_mode = params.keyword_filter_mode.clone();

            let watch_later = watch_later::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                path: sea_orm::Set(params.path.clone()),
                created_at: sea_orm::Set(crate::utils::time_format::now_standard_string()),
                latest_row_at: sea_orm::Set(crate::utils::time_format::now_standard_string()),
                enabled: sea_orm::Set(true),
                scan_deleted_videos: sea_orm::Set(false),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode),
                blacklist_keywords: sea_orm::Set(None),
                whitelist_keywords: sea_orm::Set(None),
                keyword_case_sensitive: sea_orm::Set(true),
                audio_only: sea_orm::Set(params.audio_only.unwrap_or(false)),
                download_danmaku: sea_orm::Set(params.download_danmaku.unwrap_or(true)),
                download_subtitle: sea_orm::Set(params.download_subtitle.unwrap_or(true)),
                ai_rename: sea_orm::Set(params.ai_rename.unwrap_or(false)),
                ai_rename_video_prompt: sea_orm::Set(params.ai_rename_video_prompt.clone().unwrap_or_default()),
                ai_rename_audio_prompt: sea_orm::Set(params.ai_rename_audio_prompt.clone().unwrap_or_default()),
                ai_rename_enable_multi_page: sea_orm::Set(params.ai_rename_enable_multi_page.unwrap_or(false)),
                ai_rename_enable_collection: sea_orm::Set(params.ai_rename_enable_collection.unwrap_or(false)),
                ai_rename_enable_bangumi: sea_orm::Set(params.ai_rename_enable_bangumi.unwrap_or(false)),
                ai_rename_rename_parent_dir: sea_orm::Set(params.ai_rename_rename_parent_dir.unwrap_or(false)),
                audio_only_m4a_only: sea_orm::Set(params.audio_only_m4a_only.unwrap_or(false)),
                flat_folder: sea_orm::Set(params.flat_folder.unwrap_or(false)),
            };

            let insert_result = watch_later::Entity::insert(watch_later).exec(&txn).await?;

            info!("稍后观看添加成功，保存路径: {}", params.path);

            AddVideoSourceResponse {
                success: true,
                source_id: insert_result.last_insert_id,
                source_type: "watch_later".to_string(),
                message: "稍后观看添加成功".to_string(),
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", params.source_type).into()),
    };

    // 确保目标路径存在
    std::fs::create_dir_all(&params.path).map_err(|e| anyhow!("创建目录失败: {}", e))?;

    txn.commit().await?;

    Ok(result)
}

/// 重新加载配置
#[utoipa::path(
    post,
    path = "/api/reload-config",
    responses(
        (status = 200, body = ApiResponse<bool>),
    )
)]
pub async fn reload_config(Extension(db): Extension<Arc<DatabaseConnection>>) -> Result<ApiResponse<bool>, ApiError> {
    // 检查是否正在扫描
    if crate::task::is_scanning() {
        // 正在扫描，将重载配置任务加入队列
        let task_id = uuid::Uuid::new_v4().to_string();
        let reload_task = crate::task::ReloadConfigTask {
            task_id: task_id.clone(),
        };

        crate::task::enqueue_reload_task(reload_task, &db).await?;

        info!("检测到正在扫描，重载配置任务已加入队列等待处理");

        return Ok(ApiResponse::ok(true));
    }

    // 没有扫描，直接执行重载配置
    match reload_config_internal().await {
        Ok(result) => Ok(ApiResponse::ok(result)),
        Err(e) => Err(e),
    }
}

/// 内部重载配置函数（用于队列处理和直接调用）
pub async fn reload_config_internal() -> Result<bool, ApiError> {
    info!("开始重新加载配置...");

    // 优先从数据库重新加载配置包
    match crate::config::reload_config_bundle().await {
        Ok(_) => {
            info!("配置包已从数据库成功重新加载并验证");
        }
        Err(e) => {
            warn!("从数据库重新加载配置包失败: {}, 回退到TOML重载", e);
            // 回退到传统的重新加载方式
            let _new_config = crate::config::reload_config();
            warn!("已回退到TOML配置重载，但某些功能可能受限");
        }
    }

    // 验证重载后的配置
    let verification_result = crate::config::with_config(|bundle| {
        use serde_json::json;
        let test_data = json!({
            "upper_name": "TestUP",
            "title": "TestVideo"
        });

        // 尝试渲染一个简单的模板以验证配置生效
        bundle.render_video_template(&test_data)
    });

    match verification_result {
        Ok(rendered_result) => {
            info!("配置重载验证成功，模板渲染结果: '{}'", rendered_result);

            // 检查是否包含路径分隔符，这有助于发现模板更改
            if rendered_result.contains("/") {
                warn!("检测到模板包含路径分隔符，这可能影响现有视频的目录结构");
                warn!("如果您刚刚更改了视频文件名模板，请注意现有视频可能需要重新处理");
                warn!("重新处理时将从视频源原始路径重新计算，确保目录结构正确");
            }

            Ok(true)
        }
        Err(e) => {
            error!("配置重载验证失败: {}", e);
            Err(ApiError::from(anyhow::anyhow!("配置重载验证失败: {}", e)))
        }
    }
}

/// 更新视频源启用状态
#[utoipa::path(
    put,
    path = "/api/video-sources/{source_type}/{id}/enabled",
    params(
        ("source_type" = String, Path, description = "视频源类型"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    request_body = crate::api::request::UpdateVideoSourceEnabledRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::UpdateVideoSourceEnabledResponse>),
    )
)]
pub async fn update_video_source_enabled(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
    axum::Json(params): axum::Json<crate::api::request::UpdateVideoSourceEnabledRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateVideoSourceEnabledResponse>, ApiError> {
    update_video_source_enabled_internal(db, source_type, id, params.enabled)
        .await
        .map(ApiResponse::ok)
}

/// 内部更新视频源启用状态函数
pub async fn update_video_source_enabled_internal(
    db: Arc<DatabaseConnection>,
    source_type: String,
    id: i32,
    enabled: bool,
) -> Result<crate::api::response::UpdateVideoSourceEnabledResponse, ApiError> {
    // 使用主数据库连接
    let txn = db.begin().await?;
    let result = match source_type.as_str() {
        "collection" => {
            let collection = collection::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            collection::Entity::update(collection::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                enabled: sea_orm::Set(enabled),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceEnabledResponse {
                success: true,
                source_id: id,
                source_type: "collection".to_string(),
                enabled,
                message: format!("合集 {} 已{}", collection.name, if enabled { "启用" } else { "禁用" }),
            }
        }
        "favorite" => {
            let favorite = favorite::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            favorite::Entity::update(favorite::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                enabled: sea_orm::Set(enabled),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceEnabledResponse {
                success: true,
                source_id: id,
                source_type: "favorite".to_string(),
                enabled,
                message: format!("收藏夹 {} 已{}", favorite.name, if enabled { "启用" } else { "禁用" }),
            }
        }
        "submission" => {
            let submission = submission::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            submission::Entity::update(submission::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                enabled: sea_orm::Set(enabled),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceEnabledResponse {
                success: true,
                source_id: id,
                source_type: "submission".to_string(),
                enabled,
                message: format!(
                    "UP主投稿 {} 已{}",
                    submission.upper_name,
                    if enabled { "启用" } else { "禁用" }
                ),
            }
        }
        "watch_later" => {
            let _watch_later = watch_later::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;

            watch_later::Entity::update(watch_later::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                enabled: sea_orm::Set(enabled),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceEnabledResponse {
                success: true,
                source_id: id,
                source_type: "watch_later".to_string(),
                enabled,
                message: format!("稍后观看已{}", if enabled { "启用" } else { "禁用" }),
            }
        }
        "bangumi" => {
            let bangumi = video_source::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            video_source::Entity::update(video_source::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                enabled: sea_orm::Set(enabled),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceEnabledResponse {
                success: true,
                source_id: id,
                source_type: "bangumi".to_string(),
                enabled,
                message: format!("番剧 {} 已{}", bangumi.name, if enabled { "启用" } else { "禁用" }),
            }
        }
        _ => {
            return Err(anyhow!("不支持的视频源类型: {}", source_type).into());
        }
    };

    txn.commit().await?;
    Ok(result)
}

/// 删除视频源
#[utoipa::path(
    delete,
    path = "/api/video-sources/{source_type}/{id}",
    params(
        ("source_type" = String, Path, description = "视频源类型"),
        ("id" = i32, Path, description = "视频源ID"),
        ("delete_local_files" = bool, Query, description = "是否删除本地文件")
    ),
    responses(
        (status = 200, body = ApiResponse<DeleteVideoSourceResponse>),
    )
)]
pub async fn delete_video_source(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
    Query(params): Query<crate::api::request::DeleteVideoSourceRequest>,
) -> Result<ApiResponse<crate::api::response::DeleteVideoSourceResponse>, ApiError> {
    let delete_local_files = params.delete_local_files;

    // 检查是否正在扫描
    if crate::task::is_scanning() {
        // 正在扫描，将删除任务加入队列
        let task_id = uuid::Uuid::new_v4().to_string();
        let delete_task = crate::task::DeleteVideoSourceTask {
            source_type: source_type.clone(),
            source_id: id,
            delete_local_files,
            task_id: task_id.clone(),
        };

        crate::task::enqueue_delete_task(delete_task, &db).await?;

        info!("检测到正在扫描，删除任务已加入队列等待处理: {} ID={}", source_type, id);

        return Ok(ApiResponse::ok(crate::api::response::DeleteVideoSourceResponse {
            success: true,
            source_id: id,
            source_type,
            message: "正在扫描中，删除任务已加入队列，将在扫描完成后自动处理".to_string(),
        }));
    }

    // 没有扫描，直接执行删除
    match delete_video_source_internal(db, source_type, id, delete_local_files).await {
        Ok(response) => Ok(ApiResponse::ok(response)),
        Err(e) => Err(e),
    }
}

/// 删除单个视频（软删除）
#[utoipa::path(
    delete,
    path = "/api/videos/{id}",
    params(
        ("id" = i32, description = "视频ID")
    ),
    responses(
        (status = 200, body = ApiResponse<DeleteVideoResponse>),
    )
)]
pub async fn delete_video(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path(id): Path<i32>,
) -> Result<ApiResponse<crate::api::response::DeleteVideoResponse>, ApiError> {
    // 检查是否正在扫描
    if crate::task::is_scanning() {
        // 正在扫描，将删除任务加入队列
        let task_id = uuid::Uuid::new_v4().to_string();
        let delete_task = crate::task::DeleteVideoTask {
            video_id: id,
            task_id: task_id.clone(),
        };

        crate::task::enqueue_video_delete_task(delete_task, &db).await?;

        info!("检测到正在扫描，视频删除任务已加入队列等待处理: 视频ID={}", id);

        return Ok(ApiResponse::ok(crate::api::response::DeleteVideoResponse {
            success: true,
            video_id: id,
            message: "正在扫描中，视频删除任务已加入队列，将在扫描完成后自动处理".to_string(),
        }));
    }

    // 没有扫描，直接执行删除
    match delete_video_internal(db, id).await {
        Ok(_) => Ok(ApiResponse::ok(crate::api::response::DeleteVideoResponse {
            success: true,
            video_id: id,
            message: "视频已成功删除".to_string(),
        })),
        Err(e) => Err(e),
    }
}

/// 内部删除视频函数（用于队列处理和直接调用）
pub async fn delete_video_internal(db: Arc<DatabaseConnection>, video_id: i32) -> Result<(), ApiError> {
    use bili_sync_entity::video;
    use sea_orm::*;

    // 检查视频是否存在
    let video = video::Entity::find_by_id(video_id).one(db.as_ref()).await?;

    let video = match video {
        Some(v) => v,
        None => {
            return Err(crate::api::error::InnerApiError::NotFound(video_id).into());
        }
    };

    // 检查是否已经删除
    if video.deleted == 1 {
        return Err(crate::api::error::InnerApiError::BadRequest("视频已经被删除".to_string()).into());
    }

    // 删除本地文件 - 根据page表中的路径精确删除
    let deleted_files = delete_video_files_from_pages(db.as_ref(), video_id).await?;

    if deleted_files > 0 {
        info!("已删除 {} 个视频文件", deleted_files);

        // 检查视频文件夹是否为空，如果为空则删除文件夹
        let normalized_video_path = normalize_file_path(&video.path);
        let video_path = std::path::Path::new(&normalized_video_path);
        if video_path.exists() {
            match tokio::fs::read_dir(&normalized_video_path).await {
                Ok(mut entries) => {
                    if entries.next_entry().await.unwrap_or(None).is_none() {
                        // 文件夹为空，删除它
                        if let Err(e) = std::fs::remove_dir(&normalized_video_path) {
                            warn!("删除空文件夹失败: {} - {}", normalized_video_path, e);
                        } else {
                            info!("已删除空文件夹: {}", normalized_video_path);
                        }
                    }
                }
                Err(e) => {
                    warn!("读取文件夹失败: {} - {}", normalized_video_path, e);
                }
            }
        }
    } else {
        debug!("未找到需要删除的文件，视频ID: {}", video_id);
    }

    // 执行软删除：将deleted字段设为1
    video::Entity::update_many()
        .col_expr(video::Column::Deleted, sea_orm::prelude::Expr::value(1))
        .filter(video::Column::Id.eq(video_id))
        .exec(db.as_ref())
        .await?;

    info!("视频已成功删除: ID={}, 名称={}", video_id, video.name);

    Ok(())
}

/// 根据page表精确删除视频文件
async fn delete_video_files_from_pages(conn: &impl ConnectionTrait, video_id: i32) -> Result<usize, ApiError> {
    use tokio::fs;

    // 获取该视频的所有页面（分P）
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(video_id))
        .all(conn)
        .await?;

    let mut deleted_count = 0;

    for page in pages {
        if let Some(file_path) = &page.path {
            let path = std::path::Path::new(file_path);
            info!("尝试删除视频文件: {}", file_path);
            if path.exists() {
                match fs::remove_file(path).await {
                    Ok(_) => {
                        debug!("已删除视频文件: {}", file_path);
                        deleted_count += 1;
                    }
                    Err(e) => {
                        warn!("删除视频文件失败: {} - {}", file_path, e);
                    }
                }
            } else {
                debug!("文件不存在，跳过删除: {}", file_path);
            }
        }

        // 同时删除封面图片（如果存在且是本地文件）
        if let Some(image_path) = &page.image {
            // 跳过HTTP URL，只处理本地文件路径
            if !image_path.starts_with("http://") && !image_path.starts_with("https://") {
                let path = std::path::Path::new(image_path);
                info!("尝试删除封面图片: {}", image_path);
                if path.exists() {
                    match fs::remove_file(path).await {
                        Ok(_) => {
                            info!("已删除封面图片: {}", image_path);
                            deleted_count += 1;
                        }
                        Err(e) => {
                            warn!("删除封面图片失败: {} - {}", image_path, e);
                        }
                    }
                } else {
                    debug!("封面图片文件不存在，跳过删除: {}", image_path);
                }
            } else {
                debug!("跳过远程封面图片URL: {}", image_path);
            }
        }
    }

    // 还要删除视频的NFO文件和其他可能的相关文件
    let video = video::Entity::find_by_id(video_id).one(conn).await?;

    if let Some(video) = video {
        // 获取页面信息来删除基于视频文件名的相关文件
        let pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(video_id))
            .all(conn)
            .await?;

        for page in &pages {
            if let Some(file_path) = &page.path {
                let video_file = std::path::Path::new(file_path);
                if let Some(parent_dir) = video_file.parent() {
                    if let Some(file_stem) = video_file.file_stem() {
                        let file_stem_str = file_stem.to_string_lossy();

                        // 删除同名的NFO文件
                        let nfo_path = parent_dir.join(format!("{}.nfo", file_stem_str));
                        if nfo_path.exists() {
                            match fs::remove_file(&nfo_path).await {
                                Ok(_) => {
                                    debug!("已删除NFO文件: {:?}", nfo_path);
                                    deleted_count += 1;
                                }
                                Err(e) => {
                                    warn!("删除NFO文件失败: {:?} - {}", nfo_path, e);
                                }
                            }
                        }

                        // 删除封面文件 (-fanart.jpg, -thumb.jpg等)
                        for suffix in &["fanart", "thumb"] {
                            for ext in &["jpg", "jpeg", "png", "webp"] {
                                let cover_path = parent_dir.join(format!("{}-{}.{}", file_stem_str, suffix, ext));
                                if cover_path.exists() {
                                    match fs::remove_file(&cover_path).await {
                                        Ok(_) => {
                                            debug!("已删除封面文件: {:?}", cover_path);
                                            deleted_count += 1;
                                        }
                                        Err(e) => {
                                            warn!("删除封面文件失败: {:?} - {}", cover_path, e);
                                        }
                                    }
                                }
                            }
                        }

                        // 删除弹幕文件 (.zh-CN.default.ass等)
                        let danmaku_patterns = [
                            format!("{}.zh-CN.default.ass", file_stem_str),
                            format!("{}.ass", file_stem_str),
                            format!("{}.srt", file_stem_str),
                            format!("{}.xml", file_stem_str),
                        ];

                        for pattern in &danmaku_patterns {
                            let danmaku_path = parent_dir.join(pattern);
                            if danmaku_path.exists() {
                                match fs::remove_file(&danmaku_path).await {
                                    Ok(_) => {
                                        debug!("已删除弹幕文件: {:?}", danmaku_path);
                                        deleted_count += 1;
                                    }
                                    Err(e) => {
                                        warn!("删除弹幕文件失败: {:?} - {}", danmaku_path, e);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Season结构检测和根目录元数据文件删除
        if !pages.is_empty() {
            // 检测是否使用Season结构：比较video.path和page.path
            if let Some(first_page) = pages.first() {
                if let Some(page_path) = &first_page.path {
                    let video_path = std::path::Path::new(&video.path);
                    let page_path = std::path::Path::new(page_path);

                    // 如果page路径包含Season文件夹，说明使用了Season结构
                    let uses_season_structure = page_path.components().any(|component| {
                        if let std::path::Component::Normal(name) = component {
                            name.to_string_lossy().starts_with("Season ")
                        } else {
                            false
                        }
                    });

                    if uses_season_structure {
                        debug!("检测到Season结构，删除根目录元数据文件");

                        // 获取配置以确定video_base_name生成规则
                        let config = crate::config::reload_config();

                        // 确定是否为合集或多P视频
                        let is_collection = video.collection_id.is_some();
                        let is_single_page = video.single_page.unwrap_or(true);

                        // 检查是否需要处理
                        let should_process = (is_collection && config.collection_use_season_structure)
                            || (!is_single_page && config.multi_page_use_season_structure);

                        if should_process {
                            let video_base_name = if is_collection && config.collection_use_season_structure {
                                // 合集：使用合集名称
                                match collection::Entity::find_by_id(video.collection_id.unwrap_or(0))
                                    .one(conn)
                                    .await
                                {
                                    Ok(Some(coll)) => coll.name,
                                    _ => "collection".to_string(),
                                }
                            } else {
                                // 多P视频：使用视频名称模板
                                use crate::utils::format_arg::video_format_args;
                                match crate::config::with_config(|bundle| {
                                    bundle.render_video_template(&video_format_args(&video))
                                }) {
                                    Ok(name) => name,
                                    Err(_) => video.name.clone(),
                                }
                            };

                            // 删除根目录的元数据文件
                            let metadata_files = [
                                "tvshow.nfo".to_string(),
                                format!("{}-thumb.jpg", video_base_name),
                                format!("{}-fanart.jpg", video_base_name),
                            ];

                            for metadata_file in &metadata_files {
                                let metadata_path = video_path.join(metadata_file);
                                if metadata_path.exists() {
                                    match fs::remove_file(&metadata_path).await {
                                        Ok(_) => {
                                            info!("已删除Season结构根目录元数据文件: {:?}", metadata_path);
                                            deleted_count += 1;
                                        }
                                        Err(e) => {
                                            warn!("删除Season结构根目录元数据文件失败: {:?} - {}", metadata_path, e);
                                        }
                                    }
                                } else {
                                    debug!("Season结构根目录元数据文件不存在: {:?}", metadata_path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(deleted_count)
}

/// 内部删除视频源函数（用于队列处理和直接调用）
pub async fn delete_video_source_internal(
    db: Arc<DatabaseConnection>,
    source_type: String,
    id: i32,
    delete_local_files: bool,
) -> Result<crate::api::response::DeleteVideoSourceResponse, ApiError> {
    // 用于保存需要清除断点的UP主ID（仅submission类型使用）
    let mut upper_id_to_clear: Option<i64> = None;

    // 使用主数据库连接
    let txn = db.begin().await?;

    // 根据不同类型的视频源执行不同的删除操作
    let result = match source_type.as_str() {
        "collection" => {
            // 查找要删除的合集
            let collection = collection::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            // 获取属于该合集的视频
            let videos = video::Entity::find()
                .filter(video::Column::CollectionId.eq(id))
                .all(&txn)
                .await?;

            // 清空合集关联，而不是直接删除视频
            video::Entity::update_many()
                .col_expr(
                    video::Column::CollectionId,
                    sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)),
                )
                .filter(video::Column::CollectionId.eq(id))
                .exec(&txn)
                .await?;

            // 找出清空关联后变成孤立的视频（所有源ID都为null）
            let orphaned_videos = video::Entity::find()
                .filter(
                    video::Column::CollectionId
                        .is_null()
                        .and(video::Column::FavoriteId.is_null())
                        .and(video::Column::WatchLaterId.is_null())
                        .and(video::Column::SubmissionId.is_null())
                        .and(video::Column::SourceId.is_null()),
                )
                .filter(video::Column::Id.is_in(videos.iter().map(|v| v.id)))
                .all(&txn)
                .await?;

            // 如果需要删除本地文件
            if delete_local_files {
                // 添加安全检查
                let base_path = &collection.path;
                if is_dangerous_path_for_deletion(base_path) {
                    warn!("检测到危险路径，跳过删除: {}", base_path);
                } else if orphaned_videos.is_empty() {
                    info!("合集 {} 没有找到需要删除的本地文件", collection.name);
                } else if collection.flat_folder {
                    info!("开始删除合集 {} 的本地文件（平铺目录）", collection.name);

                    let mut deleted_files = 0usize;
                    for video in &orphaned_videos {
                        match delete_video_files_from_pages(&txn, video.id).await {
                            Ok(count) => deleted_files += count,
                            Err(e) => warn!("删除合集视频文件失败: video_id={} - {:?}", video.id, e),
                        }
                    }

                    info!("合集 {} 删除完成，共删除 {} 个文件", collection.name, deleted_files);
                } else {
                    // 删除合集相关的具体视频文件夹，而不是删除整个合集基础目录
                    info!("开始删除合集 {} 的相关文件夹", collection.name);

                    // 获取所有相关的视频记录来确定需要删除的具体文件夹
                    let mut deleted_folders = std::collections::HashSet::new();
                    let mut total_deleted_size = 0u64;
                    let normalized_base_path = normalize_file_path(base_path).trim_end_matches('/').to_string();

                    for video in &orphaned_videos {
                        let normalized_video_path = normalize_file_path(&video.path).trim_end_matches('/').to_string();
                        if normalized_video_path == normalized_base_path {
                            warn!("检测到视频路径等于基础目录，按文件方式删除避免误删: {}", video.path);
                            if let Err(e) = delete_video_files_from_pages(&txn, video.id).await {
                                warn!("删除合集视频文件失败: video_id={} - {:?}", video.id, e);
                            }
                            continue;
                        }

                        // 对于每个视频，删除其对应的文件夹
                        let video_path = std::path::Path::new(&video.path);

                        if video_path.exists() && !deleted_folders.contains(&video.path) {
                            match get_directory_size(&video.path) {
                                Ok(size) => {
                                    let size_mb = size as f64 / 1024.0 / 1024.0;
                                    info!("删除合集视频文件夹: {} (大小: {:.2} MB)", video.path, size_mb);

                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除合集视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除合集视频文件夹: {} ({:.2} MB)", video.path, size_mb);
                                        deleted_folders.insert(video.path.clone());
                                        total_deleted_size += size;

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                                Err(e) => {
                                    warn!("无法计算文件夹大小: {} - {}", video.path, e);
                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除合集视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除合集视频文件夹: {}", video.path);
                                        deleted_folders.insert(video.path.clone());

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                            }
                        }
                    }

                    if !deleted_folders.is_empty() {
                        let total_size_mb = total_deleted_size as f64 / 1024.0 / 1024.0;
                        info!(
                            "合集 {} 删除完成，共删除 {} 个文件夹，总大小: {:.2} MB",
                            collection.name,
                            deleted_folders.len(),
                            total_size_mb
                        );
                    } else {
                        info!("合集 {} 没有找到需要删除的本地文件夹", collection.name);
                    }
                }

                // 若合集基础目录也已空，则清理它（但不向上继续删除）
                cleanup_empty_dir_if_empty(base_path, "合集基础目录");
            }

            // 删除孤立视频的页面数据
            for video in &orphaned_videos {
                page::Entity::delete_many()
                    .filter(page::Column::VideoId.eq(video.id))
                    .exec(&txn)
                    .await?;
            }

            // 删除孤立视频记录
            if !orphaned_videos.is_empty() {
                video::Entity::delete_many()
                    .filter(video::Column::Id.is_in(orphaned_videos.iter().map(|v| v.id)))
                    .exec(&txn)
                    .await?;
            }

            // 删除数据库中的记录
            collection::Entity::delete_by_id(id).exec(&txn).await?;

            crate::api::response::DeleteVideoSourceResponse {
                success: true,
                source_id: id,
                source_type: "collection".to_string(),
                message: format!("合集 {} 已成功删除", collection.name),
            }
        }
        "favorite" => {
            // 查找要删除的收藏夹
            let favorite = favorite::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            // 获取属于该收藏夹的视频
            let videos = video::Entity::find()
                .filter(video::Column::FavoriteId.eq(id))
                .all(&txn)
                .await?;

            // 清空收藏夹关联，而不是直接删除视频
            video::Entity::update_many()
                .col_expr(
                    video::Column::FavoriteId,
                    sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)),
                )
                .filter(video::Column::FavoriteId.eq(id))
                .exec(&txn)
                .await?;

            // 找出清空关联后变成孤立的视频（所有源ID都为null）
            let orphaned_videos = video::Entity::find()
                .filter(
                    video::Column::CollectionId
                        .is_null()
                        .and(video::Column::FavoriteId.is_null())
                        .and(video::Column::WatchLaterId.is_null())
                        .and(video::Column::SubmissionId.is_null())
                        .and(video::Column::SourceId.is_null()),
                )
                .filter(video::Column::Id.is_in(videos.iter().map(|v| v.id)))
                .all(&txn)
                .await?;

            // 如果需要删除本地文件
            if delete_local_files {
                let base_path = &favorite.path;
                if is_dangerous_path_for_deletion(base_path) {
                    warn!("检测到危险路径，跳过删除: {}", base_path);
                } else if orphaned_videos.is_empty() {
                    info!("收藏夹 {} 没有找到需要删除的本地文件", favorite.name);
                } else if favorite.flat_folder {
                    info!("开始删除收藏夹 {} 的本地文件（平铺目录）", favorite.name);

                    let mut deleted_files = 0usize;
                    for video in &orphaned_videos {
                        match delete_video_files_from_pages(&txn, video.id).await {
                            Ok(count) => deleted_files += count,
                            Err(e) => warn!("删除收藏夹视频文件失败: video_id={} - {:?}", video.id, e),
                        }
                    }

                    info!("收藏夹 {} 删除完成，共删除 {} 个文件", favorite.name, deleted_files);
                } else {
                    // 删除收藏夹相关的具体视频文件夹，而不是删除整个收藏夹基础目录
                    info!("开始删除收藏夹 {} 的相关文件夹", favorite.name);

                    // 获取所有相关的视频记录来确定需要删除的具体文件夹
                    let mut deleted_folders = std::collections::HashSet::new();
                    let mut total_deleted_size = 0u64;
                    let normalized_base_path = normalize_file_path(base_path).trim_end_matches('/').to_string();

                    for video in &orphaned_videos {
                        let normalized_video_path = normalize_file_path(&video.path).trim_end_matches('/').to_string();
                        if normalized_video_path == normalized_base_path {
                            warn!("检测到视频路径等于基础目录，按文件方式删除避免误删: {}", video.path);
                            if let Err(e) = delete_video_files_from_pages(&txn, video.id).await {
                                warn!("删除收藏夹视频文件失败: video_id={} - {:?}", video.id, e);
                            }
                            continue;
                        }

                        // 对于每个视频，删除其对应的文件夹
                        let video_path = std::path::Path::new(&video.path);

                        if video_path.exists() && !deleted_folders.contains(&video.path) {
                            match get_directory_size(&video.path) {
                                Ok(size) => {
                                    let size_mb = size as f64 / 1024.0 / 1024.0;
                                    info!("删除收藏夹视频文件夹: {} (大小: {:.2} MB)", video.path, size_mb);

                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除收藏夹视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除收藏夹视频文件夹: {} ({:.2} MB)", video.path, size_mb);
                                        deleted_folders.insert(video.path.clone());
                                        total_deleted_size += size;

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                                Err(e) => {
                                    warn!("无法计算文件夹大小: {} - {}", video.path, e);
                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除收藏夹视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除收藏夹视频文件夹: {}", video.path);
                                        deleted_folders.insert(video.path.clone());

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                            }
                        }
                    }

                    if !deleted_folders.is_empty() {
                        let total_size_mb = total_deleted_size as f64 / 1024.0 / 1024.0;
                        info!(
                            "收藏夹 {} 删除完成，共删除 {} 个文件夹，总大小: {:.2} MB",
                            favorite.name,
                            deleted_folders.len(),
                            total_size_mb
                        );
                    } else {
                        info!("收藏夹 {} 没有找到需要删除的本地文件夹", favorite.name);
                    }
                }

                // 若收藏夹基础目录也已空，则清理它（但不向上继续删除）
                cleanup_empty_dir_if_empty(base_path, "收藏夹基础目录");
            }

            // 删除孤立视频的页面数据
            for video in &orphaned_videos {
                page::Entity::delete_many()
                    .filter(page::Column::VideoId.eq(video.id))
                    .exec(&txn)
                    .await?;
            }

            // 删除孤立视频记录
            if !orphaned_videos.is_empty() {
                video::Entity::delete_many()
                    .filter(video::Column::Id.is_in(orphaned_videos.iter().map(|v| v.id)))
                    .exec(&txn)
                    .await?;
            }

            // 删除数据库中的记录
            favorite::Entity::delete_by_id(id).exec(&txn).await?;

            crate::api::response::DeleteVideoSourceResponse {
                success: true,
                source_id: id,
                source_type: "favorite".to_string(),
                message: format!("收藏夹 {} 已成功删除", favorite.name),
            }
        }
        "submission" => {
            // 查找要删除的UP主投稿
            let submission = submission::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            // 保存upper_id用于后续清除断点
            upper_id_to_clear = Some(submission.upper_id);

            // 获取属于该UP主投稿的视频
            let videos = video::Entity::find()
                .filter(video::Column::SubmissionId.eq(id))
                .all(&txn)
                .await?;

            // 清空UP主投稿关联，而不是直接删除视频
            video::Entity::update_many()
                .col_expr(
                    video::Column::SubmissionId,
                    sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)),
                )
                .filter(video::Column::SubmissionId.eq(id))
                .exec(&txn)
                .await?;

            // 找出清空关联后变成孤立的视频（所有源ID都为null）
            let orphaned_videos = video::Entity::find()
                .filter(
                    video::Column::CollectionId
                        .is_null()
                        .and(video::Column::FavoriteId.is_null())
                        .and(video::Column::WatchLaterId.is_null())
                        .and(video::Column::SubmissionId.is_null())
                        .and(video::Column::SourceId.is_null()),
                )
                .filter(video::Column::Id.is_in(videos.iter().map(|v| v.id)))
                .all(&txn)
                .await?;

            // 如果需要删除本地文件
            if delete_local_files {
                let base_path = &submission.path;
                if is_dangerous_path_for_deletion(base_path) {
                    warn!("检测到危险路径，跳过删除: {}", base_path);
                } else if orphaned_videos.is_empty() {
                    info!("UP主投稿 {} 没有找到需要删除的本地文件", submission.upper_name);
                } else if submission.flat_folder {
                    info!("开始删除UP主投稿 {} 的本地文件（平铺目录）", submission.upper_name);

                    let mut deleted_files = 0usize;
                    for video in &orphaned_videos {
                        match delete_video_files_from_pages(&txn, video.id).await {
                            Ok(count) => deleted_files += count,
                            Err(e) => warn!("删除UP主投稿视频文件失败: video_id={} - {:?}", video.id, e),
                        }
                    }

                    info!(
                        "UP主投稿 {} 删除完成，共删除 {} 个文件",
                        submission.upper_name, deleted_files
                    );
                } else {
                    // 删除UP主投稿相关的具体视频文件夹，而不是删除整个UP主投稿基础目录
                    info!("开始删除UP主投稿 {} 的相关文件夹", submission.upper_name);

                    // 获取所有相关的视频记录来确定需要删除的具体文件夹
                    let mut deleted_folders = std::collections::HashSet::new();
                    let mut total_deleted_size = 0u64;
                    let normalized_base_path = normalize_file_path(base_path).trim_end_matches('/').to_string();

                    for video in &orphaned_videos {
                        let normalized_video_path = normalize_file_path(&video.path).trim_end_matches('/').to_string();
                        if normalized_video_path == normalized_base_path {
                            warn!("检测到视频路径等于基础目录，按文件方式删除避免误删: {}", video.path);
                            if let Err(e) = delete_video_files_from_pages(&txn, video.id).await {
                                warn!("删除UP主投稿视频文件失败: video_id={} - {:?}", video.id, e);
                            }
                            continue;
                        }

                        // 对于每个视频，删除其对应的文件夹
                        let video_path = std::path::Path::new(&video.path);

                        if video_path.exists() && !deleted_folders.contains(&video.path) {
                            match get_directory_size(&video.path) {
                                Ok(size) => {
                                    let size_mb = size as f64 / 1024.0 / 1024.0;
                                    info!("删除UP主投稿视频文件夹: {} (大小: {:.2} MB)", video.path, size_mb);

                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除UP主投稿视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除UP主投稿视频文件夹: {} ({:.2} MB)", video.path, size_mb);
                                        deleted_folders.insert(video.path.clone());
                                        total_deleted_size += size;

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                                Err(e) => {
                                    warn!("无法计算文件夹大小: {} - {}", video.path, e);
                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除UP主投稿视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除UP主投稿视频文件夹: {}", video.path);
                                        deleted_folders.insert(video.path.clone());

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                            }
                        }
                    }

                    if !deleted_folders.is_empty() {
                        let total_size_mb = total_deleted_size as f64 / 1024.0 / 1024.0;
                        info!(
                            "UP主投稿 {} 删除完成，共删除 {} 个文件夹，总大小: {:.2} MB",
                            submission.upper_name,
                            deleted_folders.len(),
                            total_size_mb
                        );
                    } else {
                        info!("UP主投稿 {} 没有找到需要删除的本地文件夹", submission.upper_name);
                    }
                }

                // 若UP主投稿基础目录也已空，则清理它（但不向上继续删除）
                cleanup_empty_dir_if_empty(base_path, "UP主投稿基础目录");
            }

            // 删除孤立视频的页面数据
            for video in &orphaned_videos {
                page::Entity::delete_many()
                    .filter(page::Column::VideoId.eq(video.id))
                    .exec(&txn)
                    .await?;
            }

            // 删除孤立视频记录
            if !orphaned_videos.is_empty() {
                video::Entity::delete_many()
                    .filter(video::Column::Id.is_in(orphaned_videos.iter().map(|v| v.id)))
                    .exec(&txn)
                    .await?;
            }

            // 删除数据库中的记录
            submission::Entity::delete_by_id(id).exec(&txn).await?;

            crate::api::response::DeleteVideoSourceResponse {
                success: true,
                source_id: id,
                source_type: "submission".to_string(),
                message: format!("UP主 {} 的投稿已成功删除", submission.upper_name),
            }
        }
        "watch_later" => {
            // 查找要删除的稍后再看
            let watch_later = watch_later::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后再看"))?;

            // 获取属于稍后再看的视频
            let videos = video::Entity::find()
                .filter(video::Column::WatchLaterId.eq(id))
                .all(&txn)
                .await?;

            // 清空稍后再看关联，而不是直接删除视频
            video::Entity::update_many()
                .col_expr(
                    video::Column::WatchLaterId,
                    sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)),
                )
                .filter(video::Column::WatchLaterId.eq(id))
                .exec(&txn)
                .await?;

            // 找出清空关联后变成孤立的视频（所有源ID都为null）
            let orphaned_videos = video::Entity::find()
                .filter(
                    video::Column::CollectionId
                        .is_null()
                        .and(video::Column::FavoriteId.is_null())
                        .and(video::Column::WatchLaterId.is_null())
                        .and(video::Column::SubmissionId.is_null())
                        .and(video::Column::SourceId.is_null()),
                )
                .filter(video::Column::Id.is_in(videos.iter().map(|v| v.id)))
                .all(&txn)
                .await?;

            // 如果需要删除本地文件
            if delete_local_files {
                let base_path = &watch_later.path;
                if is_dangerous_path_for_deletion(base_path) {
                    warn!("检测到危险路径，跳过删除: {}", base_path);
                } else if orphaned_videos.is_empty() {
                    info!("稍后再看没有找到需要删除的本地文件");
                } else if watch_later.flat_folder {
                    info!("开始删除稍后再看的本地文件（平铺目录）");

                    let mut deleted_files = 0usize;
                    for video in &orphaned_videos {
                        match delete_video_files_from_pages(&txn, video.id).await {
                            Ok(count) => deleted_files += count,
                            Err(e) => warn!("删除稍后再看视频文件失败: video_id={} - {:?}", video.id, e),
                        }
                    }

                    info!("稍后再看删除完成，共删除 {} 个文件", deleted_files);
                } else {
                    // 删除稍后再看相关的具体视频文件夹，而不是删除整个稍后再看基础目录
                    info!("开始删除稍后再看的相关文件夹");

                    // 获取所有相关的视频记录来确定需要删除的具体文件夹
                    let mut deleted_folders = std::collections::HashSet::new();
                    let mut total_deleted_size = 0u64;
                    let normalized_base_path = normalize_file_path(base_path).trim_end_matches('/').to_string();

                    for video in &orphaned_videos {
                        let normalized_video_path = normalize_file_path(&video.path).trim_end_matches('/').to_string();
                        if normalized_video_path == normalized_base_path {
                            warn!("检测到视频路径等于基础目录，按文件方式删除避免误删: {}", video.path);
                            if let Err(e) = delete_video_files_from_pages(&txn, video.id).await {
                                warn!("删除稍后再看视频文件失败: video_id={} - {:?}", video.id, e);
                            }
                            continue;
                        }

                        // 对于每个视频，删除其对应的文件夹
                        let video_path = std::path::Path::new(&video.path);

                        if video_path.exists() && !deleted_folders.contains(&video.path) {
                            match get_directory_size(&video.path) {
                                Ok(size) => {
                                    let size_mb = size as f64 / 1024.0 / 1024.0;
                                    info!("删除稍后再看视频文件夹: {} (大小: {:.2} MB)", video.path, size_mb);

                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除稍后再看视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除稍后再看视频文件夹: {} ({:.2} MB)", video.path, size_mb);
                                        deleted_folders.insert(video.path.clone());
                                        total_deleted_size += size;

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                                Err(e) => {
                                    warn!("无法计算文件夹大小: {} - {}", video.path, e);
                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除稍后再看视频文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除稍后再看视频文件夹: {}", video.path);
                                        deleted_folders.insert(video.path.clone());

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                            }
                        }
                    }

                    if !deleted_folders.is_empty() {
                        let total_size_mb = total_deleted_size as f64 / 1024.0 / 1024.0;
                        info!(
                            "稍后再看删除完成，共删除 {} 个文件夹，总大小: {:.2} MB",
                            deleted_folders.len(),
                            total_size_mb
                        );
                    } else {
                        info!("稍后再看没有找到需要删除的本地文件夹");
                    }
                }

                // 若稍后再看基础目录也已空，则清理它（但不向上继续删除）
                cleanup_empty_dir_if_empty(base_path, "稍后再看基础目录");
            }

            // 删除孤立视频的页面数据
            for video in &orphaned_videos {
                page::Entity::delete_many()
                    .filter(page::Column::VideoId.eq(video.id))
                    .exec(&txn)
                    .await?;
            }

            // 删除孤立视频记录
            if !orphaned_videos.is_empty() {
                video::Entity::delete_many()
                    .filter(video::Column::Id.is_in(orphaned_videos.iter().map(|v| v.id)))
                    .exec(&txn)
                    .await?;
            }

            // 删除数据库中的记录
            watch_later::Entity::delete_by_id(id).exec(&txn).await?;

            crate::api::response::DeleteVideoSourceResponse {
                success: true,
                source_id: id,
                source_type: "watch_later".to_string(),
                message: "稍后再看已成功删除".to_string(),
            }
        }
        "bangumi" => {
            // 查找要删除的番剧
            let bangumi = video_source::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            // 获取属于该番剧的视频
            let videos = video::Entity::find()
                .filter(video::Column::SourceId.eq(id))
                .filter(video::Column::SourceType.eq(1)) // 番剧类型
                .all(&txn)
                .await?;

            // 清空番剧关联，而不是直接删除视频
            video::Entity::update_many()
                .col_expr(
                    video::Column::SourceId,
                    sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)),
                )
                .col_expr(
                    video::Column::SourceType,
                    sea_orm::sea_query::Expr::value(sea_orm::Value::Int(None)),
                )
                .filter(video::Column::SourceId.eq(id))
                .filter(video::Column::SourceType.eq(1))
                .exec(&txn)
                .await?;

            // 找出清空关联后变成孤立的视频（所有源ID都为null）
            let orphaned_videos = video::Entity::find()
                .filter(
                    video::Column::CollectionId
                        .is_null()
                        .and(video::Column::FavoriteId.is_null())
                        .and(video::Column::WatchLaterId.is_null())
                        .and(video::Column::SubmissionId.is_null())
                        .and(video::Column::SourceId.is_null()),
                )
                .filter(video::Column::Id.is_in(videos.iter().map(|v| v.id)))
                .all(&txn)
                .await?;

            // 如果需要删除本地文件
            if delete_local_files {
                let base_path = &bangumi.path;
                if is_dangerous_path_for_deletion(base_path) {
                    warn!("检测到危险路径，跳过删除: {}", base_path);
                } else if orphaned_videos.is_empty() {
                    info!("番剧 {} 没有找到需要删除的本地文件", bangumi.name);
                } else if bangumi.flat_folder {
                    info!("开始删除番剧 {} 的本地文件（平铺目录）", bangumi.name);

                    let mut deleted_files = 0usize;
                    for video in &orphaned_videos {
                        match delete_video_files_from_pages(&txn, video.id).await {
                            Ok(count) => deleted_files += count,
                            Err(e) => warn!("删除番剧视频文件失败: video_id={} - {:?}", video.id, e),
                        }
                    }

                    info!("番剧 {} 删除完成，共删除 {} 个文件", bangumi.name, deleted_files);
                } else {
                    // 删除番剧相关的季度文件夹，而不是删除整个番剧基础目录
                    info!("开始删除番剧 {} 的相关文件夹", bangumi.name);

                    // 获取所有相关的视频记录来确定需要删除的具体文件夹
                    let mut deleted_folders = std::collections::HashSet::new();
                    let mut total_deleted_size = 0u64;
                    let normalized_base_path = normalize_file_path(base_path).trim_end_matches('/').to_string();

                    for video in &orphaned_videos {
                        let normalized_video_path = normalize_file_path(&video.path).trim_end_matches('/').to_string();
                        if normalized_video_path == normalized_base_path {
                            warn!("检测到视频路径等于基础目录，按文件方式删除避免误删: {}", video.path);
                            if let Err(e) = delete_video_files_from_pages(&txn, video.id).await {
                                warn!("删除番剧视频文件失败: video_id={} - {:?}", video.id, e);
                            }
                            continue;
                        }

                        // 对于每个视频，删除其对应的文件夹
                        let video_path = std::path::Path::new(&video.path);

                        if video_path.exists() && !deleted_folders.contains(&video.path) {
                            match get_directory_size(&video.path) {
                                Ok(size) => {
                                    let size_mb = size as f64 / 1024.0 / 1024.0;
                                    info!("删除番剧季度文件夹: {} (大小: {:.2} MB)", video.path, size_mb);

                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除番剧季度文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除番剧季度文件夹: {} ({:.2} MB)", video.path, size_mb);
                                        deleted_folders.insert(video.path.clone());
                                        total_deleted_size += size;

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                                Err(e) => {
                                    warn!("无法计算文件夹大小: {} - {}", video.path, e);
                                    if let Err(e) = std::fs::remove_dir_all(&video.path) {
                                        error!("删除番剧季度文件夹失败: {} - {}", video.path, e);
                                    } else {
                                        info!("成功删除番剧季度文件夹: {}", video.path);
                                        deleted_folders.insert(video.path.clone());

                                        // 删除后清理空的父目录
                                        cleanup_empty_parent_dirs(&video.path, base_path);
                                    }
                                }
                            }
                        }
                    }

                    if !deleted_folders.is_empty() {
                        let total_size_mb = total_deleted_size as f64 / 1024.0 / 1024.0;
                        info!(
                            "番剧 {} 删除完成，共删除 {} 个文件夹，总大小: {:.2} MB",
                            bangumi.name,
                            deleted_folders.len(),
                            total_size_mb
                        );
                    } else {
                        info!("番剧 {} 没有找到需要删除的本地文件夹", bangumi.name);
                    }
                }

                // 若番剧基础目录也已空，则清理它（但不向上继续删除）
                cleanup_empty_dir_if_empty(base_path, "番剧基础目录");
            }

            // 删除孤立视频的页面数据
            for video in &orphaned_videos {
                page::Entity::delete_many()
                    .filter(page::Column::VideoId.eq(video.id))
                    .exec(&txn)
                    .await?;
            }

            // 删除孤立视频记录
            if !orphaned_videos.is_empty() {
                video::Entity::delete_many()
                    .filter(video::Column::Id.is_in(orphaned_videos.iter().map(|v| v.id)))
                    .exec(&txn)
                    .await?;
            }

            // 删除数据库中的记录
            video_source::Entity::delete_by_id(id).exec(&txn).await?;

            crate::api::response::DeleteVideoSourceResponse {
                success: true,
                source_id: id,
                source_type: "bangumi".to_string(),
                message: format!("番剧 {} 已成功删除", bangumi.name),
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type).into()),
    };

    txn.commit().await?;

    // 事务提交后，清除断点信息（如果是删除投稿源）
    if let Some(upper_id) = upper_id_to_clear {
        if let Err(e) = crate::utils::submission_checkpoint::clear_submission_checkpoint(&db, upper_id).await {
            warn!("清除UP主 {} 断点信息失败: {}", upper_id, e);
        }
    }

    Ok(result)
}

/// 更新视频源扫描已删除视频设置
#[utoipa::path(
    put,
    path = "/api/video-sources/{source_type}/{id}/scan-deleted",
    params(
        ("source_type" = String, Path, description = "视频源类型"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    request_body = crate::api::request::UpdateVideoSourceScanDeletedRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::UpdateVideoSourceScanDeletedResponse>),
    )
)]
pub async fn update_video_source_scan_deleted(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
    axum::Json(params): axum::Json<crate::api::request::UpdateVideoSourceScanDeletedRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateVideoSourceScanDeletedResponse>, ApiError> {
    update_video_source_scan_deleted_internal(db, source_type, id, params.scan_deleted_videos)
        .await
        .map(ApiResponse::ok)
}

/// 内部更新视频源扫描已删除视频设置函数
pub async fn update_video_source_scan_deleted_internal(
    db: Arc<DatabaseConnection>,
    source_type: String,
    id: i32,
    scan_deleted_videos: bool,
) -> Result<crate::api::response::UpdateVideoSourceScanDeletedResponse, ApiError> {
    // 使用主数据库连接

    let txn = db.begin().await?;

    let result = match source_type.as_str() {
        "collection" => {
            let collection = collection::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            collection::Entity::update(collection::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                scan_deleted_videos: sea_orm::Set(scan_deleted_videos),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceScanDeletedResponse {
                success: true,
                source_id: id,
                source_type: "collection".to_string(),
                scan_deleted_videos,
                message: format!(
                    "合集 {} 的扫描已删除视频设置已{}",
                    collection.name,
                    if scan_deleted_videos { "启用" } else { "禁用" }
                ),
            }
        }
        "favorite" => {
            let favorite = favorite::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            favorite::Entity::update(favorite::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                scan_deleted_videos: sea_orm::Set(scan_deleted_videos),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceScanDeletedResponse {
                success: true,
                source_id: id,
                source_type: "favorite".to_string(),
                scan_deleted_videos,
                message: format!(
                    "收藏夹 {} 的扫描已删除视频设置已{}",
                    favorite.name,
                    if scan_deleted_videos { "启用" } else { "禁用" }
                ),
            }
        }
        "submission" => {
            let submission = submission::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            submission::Entity::update(submission::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                scan_deleted_videos: sea_orm::Set(scan_deleted_videos),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceScanDeletedResponse {
                success: true,
                source_id: id,
                source_type: "submission".to_string(),
                scan_deleted_videos,
                message: format!(
                    "UP主投稿 {} 的扫描已删除视频设置已{}",
                    submission.upper_name,
                    if scan_deleted_videos { "启用" } else { "禁用" }
                ),
            }
        }
        "watch_later" => {
            let _watch_later = watch_later::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;

            watch_later::Entity::update(watch_later::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                scan_deleted_videos: sea_orm::Set(scan_deleted_videos),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceScanDeletedResponse {
                success: true,
                source_id: id,
                source_type: "watch_later".to_string(),
                scan_deleted_videos,
                message: format!(
                    "稍后观看的扫描已删除视频设置已{}",
                    if scan_deleted_videos { "启用" } else { "禁用" }
                ),
            }
        }
        "bangumi" => {
            let video_source = video_source::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            video_source::Entity::update(video_source::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                scan_deleted_videos: sea_orm::Set(scan_deleted_videos),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceScanDeletedResponse {
                success: true,
                source_id: id,
                source_type: "bangumi".to_string(),
                scan_deleted_videos,
                message: format!(
                    "番剧 {} 的扫描已删除视频设置已{}",
                    video_source.name,
                    if scan_deleted_videos { "启用" } else { "禁用" }
                ),
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type).into()),
    };

    txn.commit().await?;
    Ok(result)
}

/// 更新视频源下载选项
#[utoipa::path(
    put,
    path = "/api/video-sources/{source_type}/{id}/download-options",
    params(
        ("source_type" = String, Path, description = "视频源类型"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    request_body = crate::api::request::UpdateVideoSourceDownloadOptionsRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::UpdateVideoSourceDownloadOptionsResponse>),
    )
)]
pub async fn update_video_source_download_options(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
    axum::Json(params): axum::Json<crate::api::request::UpdateVideoSourceDownloadOptionsRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateVideoSourceDownloadOptionsResponse>, ApiError> {
    update_video_source_download_options_internal(db, source_type, id, params)
        .await
        .map(ApiResponse::ok)
}

/// 内部更新视频源下载选项函数
pub async fn update_video_source_download_options_internal(
    db: Arc<DatabaseConnection>,
    source_type: String,
    id: i32,
    params: crate::api::request::UpdateVideoSourceDownloadOptionsRequest,
) -> Result<crate::api::response::UpdateVideoSourceDownloadOptionsResponse, ApiError> {
    let txn = db.begin().await?;

    let result = match source_type.as_str() {
        "collection" => {
            let collection = collection::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            let audio_only = params.audio_only.unwrap_or(collection.audio_only);
            let audio_only_m4a_only = params.audio_only_m4a_only.unwrap_or(collection.audio_only_m4a_only);
            let flat_folder = params.flat_folder.unwrap_or(collection.flat_folder);
            let download_danmaku = params.download_danmaku.unwrap_or(collection.download_danmaku);
            let download_subtitle = params.download_subtitle.unwrap_or(collection.download_subtitle);
            let ai_rename = params.ai_rename.unwrap_or(collection.ai_rename);
            let ai_rename_video_prompt = params
                .ai_rename_video_prompt
                .clone()
                .unwrap_or(collection.ai_rename_video_prompt.clone());
            let ai_rename_audio_prompt = params
                .ai_rename_audio_prompt
                .clone()
                .unwrap_or(collection.ai_rename_audio_prompt.clone());
            let ai_rename_enable_multi_page = params
                .ai_rename_enable_multi_page
                .unwrap_or(collection.ai_rename_enable_multi_page);
            let ai_rename_enable_collection = params
                .ai_rename_enable_collection
                .unwrap_or(collection.ai_rename_enable_collection);
            let ai_rename_enable_bangumi = params
                .ai_rename_enable_bangumi
                .unwrap_or(collection.ai_rename_enable_bangumi);
            let ai_rename_rename_parent_dir = params
                .ai_rename_rename_parent_dir
                .unwrap_or(collection.ai_rename_rename_parent_dir);

            collection::Entity::update(collection::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                audio_only: sea_orm::Set(audio_only),
                audio_only_m4a_only: sea_orm::Set(audio_only_m4a_only),
                flat_folder: sea_orm::Set(flat_folder),
                download_danmaku: sea_orm::Set(download_danmaku),
                download_subtitle: sea_orm::Set(download_subtitle),
                ai_rename: sea_orm::Set(ai_rename),
                ai_rename_video_prompt: sea_orm::Set(ai_rename_video_prompt.clone()),
                ai_rename_audio_prompt: sea_orm::Set(ai_rename_audio_prompt.clone()),
                ai_rename_enable_multi_page: sea_orm::Set(ai_rename_enable_multi_page),
                ai_rename_enable_collection: sea_orm::Set(ai_rename_enable_collection),
                ai_rename_enable_bangumi: sea_orm::Set(ai_rename_enable_bangumi),
                ai_rename_rename_parent_dir: sea_orm::Set(ai_rename_rename_parent_dir),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceDownloadOptionsResponse {
                success: true,
                source_id: id,
                source_type: "collection".to_string(),
                audio_only,
                audio_only_m4a_only,
                flat_folder,
                download_danmaku,
                download_subtitle,
                ai_rename,
                ai_rename_video_prompt,
                ai_rename_audio_prompt,
                ai_rename_enable_multi_page,
                ai_rename_enable_collection,
                ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir,
                use_dynamic_api: false,
                message: format!("合集 {} 的下载选项已更新", collection.name),
            }
        }
        "favorite" => {
            let favorite = favorite::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            let audio_only = params.audio_only.unwrap_or(favorite.audio_only);
            let audio_only_m4a_only = params.audio_only_m4a_only.unwrap_or(favorite.audio_only_m4a_only);
            let flat_folder = params.flat_folder.unwrap_or(favorite.flat_folder);
            let download_danmaku = params.download_danmaku.unwrap_or(favorite.download_danmaku);
            let download_subtitle = params.download_subtitle.unwrap_or(favorite.download_subtitle);
            let ai_rename = params.ai_rename.unwrap_or(favorite.ai_rename);
            let ai_rename_video_prompt = params
                .ai_rename_video_prompt
                .clone()
                .unwrap_or(favorite.ai_rename_video_prompt.clone());
            let ai_rename_audio_prompt = params
                .ai_rename_audio_prompt
                .clone()
                .unwrap_or(favorite.ai_rename_audio_prompt.clone());
            let ai_rename_enable_multi_page = params
                .ai_rename_enable_multi_page
                .unwrap_or(favorite.ai_rename_enable_multi_page);
            let ai_rename_enable_collection = params
                .ai_rename_enable_collection
                .unwrap_or(favorite.ai_rename_enable_collection);
            let ai_rename_enable_bangumi = params
                .ai_rename_enable_bangumi
                .unwrap_or(favorite.ai_rename_enable_bangumi);
            let ai_rename_rename_parent_dir = params
                .ai_rename_rename_parent_dir
                .unwrap_or(favorite.ai_rename_rename_parent_dir);

            favorite::Entity::update(favorite::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                audio_only: sea_orm::Set(audio_only),
                audio_only_m4a_only: sea_orm::Set(audio_only_m4a_only),
                flat_folder: sea_orm::Set(flat_folder),
                download_danmaku: sea_orm::Set(download_danmaku),
                download_subtitle: sea_orm::Set(download_subtitle),
                ai_rename: sea_orm::Set(ai_rename),
                ai_rename_video_prompt: sea_orm::Set(ai_rename_video_prompt.clone()),
                ai_rename_audio_prompt: sea_orm::Set(ai_rename_audio_prompt.clone()),
                ai_rename_enable_multi_page: sea_orm::Set(ai_rename_enable_multi_page),
                ai_rename_enable_collection: sea_orm::Set(ai_rename_enable_collection),
                ai_rename_enable_bangumi: sea_orm::Set(ai_rename_enable_bangumi),
                ai_rename_rename_parent_dir: sea_orm::Set(ai_rename_rename_parent_dir),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceDownloadOptionsResponse {
                success: true,
                source_id: id,
                source_type: "favorite".to_string(),
                audio_only,
                audio_only_m4a_only,
                flat_folder,
                download_danmaku,
                download_subtitle,
                ai_rename,
                ai_rename_video_prompt,
                ai_rename_audio_prompt,
                ai_rename_enable_multi_page,
                ai_rename_enable_collection,
                ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir,
                use_dynamic_api: false,
                message: format!("收藏夹 {} 的下载选项已更新", favorite.name),
            }
        }
        "submission" => {
            let submission = submission::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            let audio_only = params.audio_only.unwrap_or(submission.audio_only);
            let audio_only_m4a_only = params.audio_only_m4a_only.unwrap_or(submission.audio_only_m4a_only);
            let flat_folder = params.flat_folder.unwrap_or(submission.flat_folder);
            let download_danmaku = params.download_danmaku.unwrap_or(submission.download_danmaku);
            let download_subtitle = params.download_subtitle.unwrap_or(submission.download_subtitle);
            let ai_rename = params.ai_rename.unwrap_or(submission.ai_rename);
            let ai_rename_video_prompt = params
                .ai_rename_video_prompt
                .clone()
                .unwrap_or(submission.ai_rename_video_prompt.clone());
            let ai_rename_audio_prompt = params
                .ai_rename_audio_prompt
                .clone()
                .unwrap_or(submission.ai_rename_audio_prompt.clone());
            let ai_rename_enable_multi_page = params
                .ai_rename_enable_multi_page
                .unwrap_or(submission.ai_rename_enable_multi_page);
            let ai_rename_enable_collection = params
                .ai_rename_enable_collection
                .unwrap_or(submission.ai_rename_enable_collection);
            let ai_rename_enable_bangumi = params
                .ai_rename_enable_bangumi
                .unwrap_or(submission.ai_rename_enable_bangumi);
            let ai_rename_rename_parent_dir = params
                .ai_rename_rename_parent_dir
                .unwrap_or(submission.ai_rename_rename_parent_dir);
            let use_dynamic_api = params.use_dynamic_api.unwrap_or(submission.use_dynamic_api);
            let mut dynamic_api_full_synced = submission.dynamic_api_full_synced;
            let mut latest_row_at_override: Option<String> = None;

            if use_dynamic_api && !submission.use_dynamic_api && !submission.dynamic_api_full_synced {
                latest_row_at_override = Some("1970-01-01 00:00:00".to_string());
                dynamic_api_full_synced = true;
                info!(
                    "UP主投稿 {} 首次启用动态API，已重置最新时间用于全量拉取",
                    submission.upper_name
                );
            }

            let mut update_model = submission::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                audio_only: sea_orm::Set(audio_only),
                audio_only_m4a_only: sea_orm::Set(audio_only_m4a_only),
                flat_folder: sea_orm::Set(flat_folder),
                download_danmaku: sea_orm::Set(download_danmaku),
                download_subtitle: sea_orm::Set(download_subtitle),
                ai_rename: sea_orm::Set(ai_rename),
                ai_rename_video_prompt: sea_orm::Set(ai_rename_video_prompt.clone()),
                ai_rename_audio_prompt: sea_orm::Set(ai_rename_audio_prompt.clone()),
                ai_rename_enable_multi_page: sea_orm::Set(ai_rename_enable_multi_page),
                ai_rename_enable_collection: sea_orm::Set(ai_rename_enable_collection),
                ai_rename_enable_bangumi: sea_orm::Set(ai_rename_enable_bangumi),
                ai_rename_rename_parent_dir: sea_orm::Set(ai_rename_rename_parent_dir),
                use_dynamic_api: sea_orm::Set(use_dynamic_api),
                dynamic_api_full_synced: sea_orm::Set(dynamic_api_full_synced),
                ..Default::default()
            };

            if let Some(latest_row_at) = latest_row_at_override {
                update_model.latest_row_at = sea_orm::Set(latest_row_at);
            }

            submission::Entity::update(update_model).exec(&txn).await?;

            crate::api::response::UpdateVideoSourceDownloadOptionsResponse {
                success: true,
                source_id: id,
                source_type: "submission".to_string(),
                audio_only,
                audio_only_m4a_only,
                flat_folder,
                download_danmaku,
                download_subtitle,
                ai_rename,
                ai_rename_video_prompt,
                ai_rename_audio_prompt,
                ai_rename_enable_multi_page,
                ai_rename_enable_collection,
                ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir,
                use_dynamic_api,
                message: format!("UP主投稿 {} 的下载选项已更新", submission.upper_name),
            }
        }
        "watch_later" => {
            let watch_later = watch_later::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;

            let audio_only = params.audio_only.unwrap_or(watch_later.audio_only);
            let audio_only_m4a_only = params.audio_only_m4a_only.unwrap_or(watch_later.audio_only_m4a_only);
            let flat_folder = params.flat_folder.unwrap_or(watch_later.flat_folder);
            let download_danmaku = params.download_danmaku.unwrap_or(watch_later.download_danmaku);
            let download_subtitle = params.download_subtitle.unwrap_or(watch_later.download_subtitle);
            let ai_rename = params.ai_rename.unwrap_or(watch_later.ai_rename);
            let ai_rename_video_prompt = params
                .ai_rename_video_prompt
                .clone()
                .unwrap_or(watch_later.ai_rename_video_prompt.clone());
            let ai_rename_audio_prompt = params
                .ai_rename_audio_prompt
                .clone()
                .unwrap_or(watch_later.ai_rename_audio_prompt.clone());
            let ai_rename_enable_multi_page = params
                .ai_rename_enable_multi_page
                .unwrap_or(watch_later.ai_rename_enable_multi_page);
            let ai_rename_enable_collection = params
                .ai_rename_enable_collection
                .unwrap_or(watch_later.ai_rename_enable_collection);
            let ai_rename_enable_bangumi = params
                .ai_rename_enable_bangumi
                .unwrap_or(watch_later.ai_rename_enable_bangumi);
            let ai_rename_rename_parent_dir = params
                .ai_rename_rename_parent_dir
                .unwrap_or(watch_later.ai_rename_rename_parent_dir);

            watch_later::Entity::update(watch_later::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                audio_only: sea_orm::Set(audio_only),
                audio_only_m4a_only: sea_orm::Set(audio_only_m4a_only),
                flat_folder: sea_orm::Set(flat_folder),
                download_danmaku: sea_orm::Set(download_danmaku),
                download_subtitle: sea_orm::Set(download_subtitle),
                ai_rename: sea_orm::Set(ai_rename),
                ai_rename_video_prompt: sea_orm::Set(ai_rename_video_prompt.clone()),
                ai_rename_audio_prompt: sea_orm::Set(ai_rename_audio_prompt.clone()),
                ai_rename_enable_multi_page: sea_orm::Set(ai_rename_enable_multi_page),
                ai_rename_enable_collection: sea_orm::Set(ai_rename_enable_collection),
                ai_rename_enable_bangumi: sea_orm::Set(ai_rename_enable_bangumi),
                ai_rename_rename_parent_dir: sea_orm::Set(ai_rename_rename_parent_dir),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceDownloadOptionsResponse {
                success: true,
                source_id: id,
                source_type: "watch_later".to_string(),
                audio_only,
                audio_only_m4a_only,
                flat_folder,
                download_danmaku,
                download_subtitle,
                ai_rename,
                ai_rename_video_prompt,
                ai_rename_audio_prompt,
                ai_rename_enable_multi_page,
                ai_rename_enable_collection,
                ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir,
                use_dynamic_api: false,
                message: "稍后观看的下载选项已更新".to_string(),
            }
        }
        "bangumi" => {
            let video_source = video_source::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            let audio_only = params.audio_only.unwrap_or(video_source.audio_only);
            let audio_only_m4a_only = params.audio_only_m4a_only.unwrap_or(video_source.audio_only_m4a_only);
            let flat_folder = params.flat_folder.unwrap_or(video_source.flat_folder);
            let download_danmaku = params.download_danmaku.unwrap_or(video_source.download_danmaku);
            let download_subtitle = params.download_subtitle.unwrap_or(video_source.download_subtitle);
            let ai_rename = params.ai_rename.unwrap_or(video_source.ai_rename);
            let ai_rename_video_prompt = params
                .ai_rename_video_prompt
                .clone()
                .unwrap_or(video_source.ai_rename_video_prompt.clone());
            let ai_rename_audio_prompt = params
                .ai_rename_audio_prompt
                .clone()
                .unwrap_or(video_source.ai_rename_audio_prompt.clone());
            let ai_rename_enable_multi_page = params
                .ai_rename_enable_multi_page
                .unwrap_or(video_source.ai_rename_enable_multi_page);
            let ai_rename_enable_collection = params
                .ai_rename_enable_collection
                .unwrap_or(video_source.ai_rename_enable_collection);
            let ai_rename_enable_bangumi = params
                .ai_rename_enable_bangumi
                .unwrap_or(video_source.ai_rename_enable_bangumi);
            let ai_rename_rename_parent_dir = params
                .ai_rename_rename_parent_dir
                .unwrap_or(video_source.ai_rename_rename_parent_dir);

            video_source::Entity::update(video_source::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                audio_only: sea_orm::Set(audio_only),
                audio_only_m4a_only: sea_orm::Set(audio_only_m4a_only),
                flat_folder: sea_orm::Set(flat_folder),
                download_danmaku: sea_orm::Set(download_danmaku),
                download_subtitle: sea_orm::Set(download_subtitle),
                ai_rename: sea_orm::Set(ai_rename),
                ai_rename_video_prompt: sea_orm::Set(ai_rename_video_prompt.clone()),
                ai_rename_audio_prompt: sea_orm::Set(ai_rename_audio_prompt.clone()),
                ai_rename_enable_multi_page: sea_orm::Set(ai_rename_enable_multi_page),
                ai_rename_enable_collection: sea_orm::Set(ai_rename_enable_collection),
                ai_rename_enable_bangumi: sea_orm::Set(ai_rename_enable_bangumi),
                ai_rename_rename_parent_dir: sea_orm::Set(ai_rename_rename_parent_dir),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateVideoSourceDownloadOptionsResponse {
                success: true,
                source_id: id,
                source_type: "bangumi".to_string(),
                audio_only,
                audio_only_m4a_only,
                flat_folder,
                download_danmaku,
                download_subtitle,
                ai_rename,
                ai_rename_video_prompt,
                ai_rename_audio_prompt,
                ai_rename_enable_multi_page,
                ai_rename_enable_collection,
                ai_rename_enable_bangumi,
                ai_rename_rename_parent_dir,
                use_dynamic_api: false,
                message: format!("番剧 {} 的下载选项已更新", video_source.name),
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type).into()),
    };

    txn.commit().await?;
    Ok(result)
}

#[derive(Debug, Default)]
struct SubmissionSelectedBackfillStats {
    requested: usize,
    queued_new: usize,
    restored_deleted: usize,
    already_exists: usize,
    skipped_non_owner: usize,
    failed: usize,
}

async fn fetch_submission_video_info_by_bvid(
    bili_client: &crate::bilibili::BiliClient,
    bvid: &str,
) -> Result<(crate::bilibili::VideoInfo, i64)> {
    let mut response = bili_client
        .request(reqwest::Method::GET, "https://api.bilibili.com/x/web-interface/view")
        .await
        .query(&[("bvid", bvid)])
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let code = response["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let message = response["message"].as_str().unwrap_or("unknown error");
        return Err(anyhow!(
            "视频详情接口返回错误(code={}): bvid={}, message={}",
            code,
            bvid,
            message
        ));
    }

    let detail = serde_json::from_value::<crate::bilibili::VideoInfo>(response["data"].take())
        .with_context(|| format!("解析视频详情失败: bvid={}", bvid))?;

    match detail {
        crate::bilibili::VideoInfo::Detail {
            title,
            bvid,
            intro,
            cover,
            upper,
            ctime,
            ugc_season,
            ..
        } => Ok((
            crate::bilibili::VideoInfo::Submission {
                title,
                bvid,
                intro,
                cover,
                ctime,
                season_id: ugc_season.and_then(|season| season.id),
            },
            upper.mid,
        )),
        _ => Err(anyhow!("视频详情结构异常，无法用于投稿回补: bvid={}", bvid)),
    }
}

async fn backfill_submission_selected_videos(
    db: &DatabaseConnection,
    submission_record: &submission::Model,
    selected_bvids: &[String],
) -> Result<SubmissionSelectedBackfillStats> {
    let mut stats = SubmissionSelectedBackfillStats::default();
    if selected_bvids.is_empty() {
        return Ok(stats);
    }

    let mut normalized_bvids: Vec<String> = selected_bvids
        .iter()
        .map(|bvid| bvid.trim().to_string())
        .filter(|bvid| !bvid.is_empty())
        .collect();
    normalized_bvids.sort();
    normalized_bvids.dedup();
    stats.requested = normalized_bvids.len();

    if normalized_bvids.is_empty() {
        return Ok(stats);
    }

    let existing_models = video::Entity::find()
        .filter(video::Column::SubmissionId.eq(submission_record.id))
        .filter(video::Column::Bvid.is_in(normalized_bvids.clone()))
        .all(db)
        .await?;
    let existing_map: HashMap<String, video::Model> = existing_models
        .into_iter()
        .map(|model| (model.bvid.clone(), model))
        .collect();

    let bili_client = crate::bilibili::BiliClient::new(String::new());
    let source_enum = crate::adapter::VideoSourceEnum::Submission(submission_record.clone());
    let mut videos_to_create: Vec<crate::bilibili::VideoInfo> = Vec::new();
    let mut pending_insert_bvids: Vec<String> = Vec::new();

    for bvid in normalized_bvids {
        if let Some(existing_video) = existing_map.get(&bvid) {
            if existing_video.deleted != 0 {
                video::Entity::update(video::ActiveModel {
                    id: Unchanged(existing_video.id),
                    deleted: Set(0),
                    download_status: Set(0),
                    path: Set(String::new()),
                    single_page: Set(None),
                    auto_download: Set(true),
                    cid: Set(None),
                    ..Default::default()
                })
                .exec(db)
                .await?;

                page::Entity::delete_many()
                    .filter(page::Column::VideoId.eq(existing_video.id))
                    .exec(db)
                    .await?;

                stats.restored_deleted += 1;
                info!(
                    "历史投稿精准回补：恢复已删除视频 {} ({})，将重新进入详情与下载流程",
                    existing_video.name, existing_video.bvid
                );
            } else {
                if !existing_video.auto_download {
                    video::Entity::update(video::ActiveModel {
                        id: Unchanged(existing_video.id),
                        auto_download: Set(true),
                        ..Default::default()
                    })
                    .exec(db)
                    .await?;
                }
                stats.already_exists += 1;
            }
            continue;
        }

        match fetch_submission_video_info_by_bvid(&bili_client, &bvid).await {
            Ok((video_info, owner_mid)) => {
                if owner_mid != submission_record.upper_id {
                    stats.skipped_non_owner += 1;
                    warn!(
                        "历史投稿精准回补跳过：{} 不属于当前UP主 {}（owner_mid={}）",
                        bvid, submission_record.upper_name, owner_mid
                    );
                    continue;
                }
                videos_to_create.push(video_info);
                pending_insert_bvids.push(bvid.clone());
            }
            Err(err) => {
                stats.failed += 1;
                warn!("历史投稿精准回补失败：{} -> {}", bvid, err);
            }
        }
    }

    if !videos_to_create.is_empty() {
        crate::utils::model::create_videos(videos_to_create, &source_enum, db).await?;
        let queued_count = video::Entity::find()
            .filter(video::Column::SubmissionId.eq(submission_record.id))
            .filter(video::Column::Bvid.is_in(pending_insert_bvids.clone()))
            .filter(video::Column::Deleted.eq(0))
            .count(db)
            .await? as usize;
        stats.queued_new = queued_count;

        let skipped_after_backfill = pending_insert_bvids.len().saturating_sub(queued_count);
        if skipped_after_backfill > 0 {
            warn!(
                "历史投稿精准回补：{} 个视频未成功入队（可能被关键词过滤或选择条件限制）",
                skipped_after_backfill
            );
        }
    }

    Ok(stats)
}

#[derive(Debug, Default)]
struct SubmissionWhitelistBackfillStats {
    total_keywords: usize,
    searched_keywords: usize,
    skipped_regex_keywords: usize,
    matched_bvids: usize,
    backfill: SubmissionSelectedBackfillStats,
}

fn is_plain_submission_search_keyword(pattern: &str) -> bool {
    // B站搜索接口不支持正则；包含正则元字符时按“无法精准搜索”处理
    const REGEX_META_CHARS: [char; 15] = [
        '\\', '^', '$', '.', '|', '?', '*', '+', '(', ')', '[', ']', '{', '}', '#',
    ];
    !pattern.chars().any(|c| REGEX_META_CHARS.contains(&c))
}

fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

fn normalize_submission_search_text(input: &str) -> String {
    let stripped = strip_html_tags(input);
    let decoded = decode_html_entities(&stripped).to_string();

    decoded
        .replace(['－', '—', '–', '―', '‐', '﹣'], "-")
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

async fn collect_submission_bvids_by_whitelist_keywords(
    upper_id: i64,
    whitelist_keywords: &[String],
) -> Result<(Vec<String>, usize, usize)> {
    let bili_client = crate::bilibili::BiliClient::new(String::new());
    let mut bvid_set: HashSet<String> = HashSet::new();
    let mut searched_keywords = 0usize;
    let mut skipped_regex_keywords = 0usize;

    for raw_keyword in whitelist_keywords {
        let keyword = raw_keyword.trim();
        if keyword.is_empty() {
            continue;
        }
        let normalized_keyword = normalize_submission_search_text(keyword);
        if normalized_keyword.is_empty() {
            continue;
        }

        if !is_plain_submission_search_keyword(keyword) {
            skipped_regex_keywords += 1;
            continue;
        }

        searched_keywords += 1;
        let mut page = 1i32;
        let page_size = 50i32;

        loop {
            let (videos, total) = bili_client
                .search_user_submission_videos(upper_id, keyword, page, page_size)
                .await
                .with_context(|| format!("白名单关键词搜索失败: up_id={}, keyword={}", upper_id, keyword))?;

            if videos.is_empty() {
                break;
            }

            for video in videos {
                let bvid = video.bvid.trim();
                let normalized_title = normalize_submission_search_text(&video.title);
                if !normalized_title.contains(&normalized_keyword) {
                    debug!(
                        "白名单关键词二次匹配未命中，跳过: up_id={}, keyword={}, title={}, bvid={}",
                        upper_id, keyword, video.title, video.bvid
                    );
                    continue;
                }
                if !bvid.is_empty() {
                    bvid_set.insert(bvid.to_string());
                }
            }

            if (page as i64) * (page_size as i64) >= total {
                break;
            }

            page += 1;
            if page > 200 {
                warn!(
                    "白名单关键词搜索达到分页上限，提前停止: up_id={}, keyword={}",
                    upper_id, keyword
                );
                break;
            }
        }
    }

    let mut bvids: Vec<String> = bvid_set.into_iter().collect();
    bvids.sort();
    Ok((bvids, searched_keywords, skipped_regex_keywords))
}

async fn backfill_submission_by_whitelist_keywords(
    db: &DatabaseConnection,
    submission_record: &submission::Model,
    whitelist_keywords: &[String],
) -> Result<SubmissionWhitelistBackfillStats> {
    let mut stats = SubmissionWhitelistBackfillStats::default();
    stats.total_keywords = whitelist_keywords
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .count();

    if stats.total_keywords == 0 {
        return Ok(stats);
    }

    let (bvids, searched_keywords, skipped_regex_keywords) =
        collect_submission_bvids_by_whitelist_keywords(submission_record.upper_id, whitelist_keywords).await?;
    stats.searched_keywords = searched_keywords;
    stats.skipped_regex_keywords = skipped_regex_keywords;
    stats.matched_bvids = bvids.len();

    if !bvids.is_empty() {
        stats.backfill = backfill_submission_selected_videos(db, submission_record, &bvids).await?;
    }

    Ok(stats)
}

/// 更新投稿源选中视频列表
#[utoipa::path(
    put,
    path = "/api/video-sources/submission/{id}/selected-videos",
    params(
        ("id" = i32, Path, description = "投稿源ID"),
    ),
    request_body = crate::api::request::UpdateSubmissionSelectedVideosRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::UpdateSubmissionSelectedVideosResponse>),
    )
)]
pub async fn update_submission_selected_videos(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path(id): Path<i32>,
    axum::Json(params): axum::Json<crate::api::request::UpdateSubmissionSelectedVideosRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateSubmissionSelectedVideosResponse>, ApiError> {
    let txn = db.begin().await?;

    // 查找投稿源
    let submission_record = submission::Entity::find_by_id(id)
        .one(&txn)
        .await?
        .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

    let mut incoming_selected_videos = params.selected_videos.clone();
    incoming_selected_videos.sort();
    incoming_selected_videos.dedup();
    let selected_count = incoming_selected_videos.len();

    let mut current_selected_videos = submission_record
        .selected_videos
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .unwrap_or_default();
    current_selected_videos.sort();
    current_selected_videos.dedup();
    let selection_changed = incoming_selected_videos != current_selected_videos;
    let incoming_set: HashSet<String> = incoming_selected_videos.iter().cloned().collect();
    let current_set: HashSet<String> = current_selected_videos.iter().cloned().collect();
    let mut newly_selected_videos: Vec<String> = incoming_set.difference(&current_set).cloned().collect();
    newly_selected_videos.sort();

    // 将选中的视频列表序列化为JSON字符串存储
    let selected_videos_json = if selected_count > 0 {
        Some(serde_json::to_string(&incoming_selected_videos).unwrap_or_default())
    } else {
        None
    };

    let mut update_model = submission::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(id),
        selected_videos: sea_orm::Set(selected_videos_json),
        ..Default::default()
    };

    if selection_changed {
        // 选择变化后清空自适应扫描节流，让下一轮可立即执行增量扫描
        update_model.next_scan_at = sea_orm::Set(None);
        update_model.no_update_streak = sea_orm::Set(0);
    }

    // 更新数据库
    submission::Entity::update(update_model).exec(&txn).await?;

    txn.commit().await?;

    let mut backfill_stats = None;
    let mut backfill_error = None;
    if selection_changed && !newly_selected_videos.is_empty() {
        // 回补必须使用“更新后的选择集”，否则新增 BV 可能被旧选择集误过滤
        let mut updated_submission_record = submission_record.clone();
        updated_submission_record.selected_videos = if selected_count > 0 {
            Some(serde_json::to_string(&incoming_selected_videos).unwrap_or_default())
        } else {
            None
        };

        match backfill_submission_selected_videos(db.as_ref(), &updated_submission_record, &newly_selected_videos).await
        {
            Ok(stats) => backfill_stats = Some(stats),
            Err(err) => {
                warn!(
                    "UP主投稿 {} 历史选择精准回补失败，将仅保留增量扫描: {}",
                    submission_record.upper_name, err
                );
                backfill_error = Some(err.to_string());
            }
        }
    }

    let mut message = if selected_count > 0 {
        format!(
            "UP主投稿 {} 的历史投稿选择已更新，选中 {} 个视频",
            submission_record.upper_name, selected_count
        )
    } else {
        format!(
            "UP主投稿 {} 的历史投稿选择已清空，将下载全部投稿",
            submission_record.upper_name
        )
    };

    if selection_changed {
        message.push_str("；历史选择改为按BV精准回补，不再触发全量扫描");
    }

    if let Some(stats) = backfill_stats {
        message.push_str(&format!(
            "；本次新增选择 {} 个：新增入队 {} 个，恢复已删 {} 个，已存在 {} 个，非当前UP {} 个，失败 {} 个",
            stats.requested,
            stats.queued_new,
            stats.restored_deleted,
            stats.already_exists,
            stats.skipped_non_owner,
            stats.failed
        ));
    }

    if let Some(err) = backfill_error {
        message.push_str(&format!("；精准回补失败（{}），请稍后重试或手动重置该源", err));
    }

    info!("{}", message);

    Ok(ApiResponse::ok(
        crate::api::response::UpdateSubmissionSelectedVideosResponse {
            success: true,
            source_id: id,
            selected_count,
            message,
        },
    ))
}

/// 删除视频（软删除）
/// 重设视频源路径
#[utoipa::path(
    post,
    path = "/api/video-sources/{source_type}/{id}/reset-path",
    request_body = ResetVideoSourcePathRequest,
    responses(
        (status = 200, body = ApiResponse<ResetVideoSourcePathResponse>),
    )
)]
pub async fn reset_video_source_path(
    Path((source_type, id)): Path<(String, i32)>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(request): axum::Json<ResetVideoSourcePathRequest>,
) -> Result<ApiResponse<ResetVideoSourcePathResponse>, ApiError> {
    match reset_video_source_path_internal(db, source_type, id, request).await {
        Ok(response) => Ok(ApiResponse::ok(response)),
        Err(e) => Err(e),
    }
}

/// 验证路径重设操作的安全性
async fn validate_path_reset_safety(
    txn: &sea_orm::DatabaseTransaction,
    source_type: &str,
    id: i32,
    new_base_path: &str,
) -> Result<(), ApiError> {
    use std::path::Path;

    // 检查新路径是否有效
    let new_path = Path::new(new_base_path);
    if !new_path.is_absolute() {
        return Err(anyhow!("新路径必须是绝对路径: {}", new_base_path).into());
    }

    // 对于番剧，进行特殊验证
    if source_type == "bangumi" {
        // 获取番剧的一个示例视频进行路径预测试
        let sample_video = video::Entity::find()
            .filter(video::Column::SourceId.eq(id))
            .filter(video::Column::SourceType.eq(1)) // 番剧类型
            .one(txn)
            .await?;

        if let Some(video) = sample_video {
            // 尝试预生成路径，检查是否会产生合理的结果
            let temp_page = bili_sync_entity::page::Model {
                id: 0,
                video_id: video.id,
                cid: 0,
                pid: 1,
                name: "temp".to_string(),
                width: None,
                height: None,
                duration: 0,
                path: None,
                image: None,
                download_status: 0,
                created_at: now_standard_string(),
                play_video_streams: None,
                play_audio_streams: None,
                play_subtitle_streams: None,
                play_streams_updated_at: None,
                ai_renamed: None,
            };

            let api_title = if let Some(current_path) = std::path::Path::new(&video.path).parent() {
                // 从当前路径中提取番剧名称（去掉Season部分）
                if let Some(folder_name) = current_path.file_name().and_then(|n| n.to_str()) {
                    // 如果当前文件夹名不是"Season XX"格式，那就是番剧名称
                    if !folder_name.starts_with("Season ") {
                        Some(folder_name.to_string())
                    } else if let Some(series_folder) = current_path.parent() {
                        // 如果当前是Season文件夹，则取其父文件夹名称
                        series_folder
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let format_args =
                crate::utils::format_arg::bangumi_page_format_args(&video, &temp_page, api_title.as_deref());
            let series_title = format_args["series_title"].as_str().unwrap_or("");

            // 验证是否会产生合理的番剧标题
            if series_title.is_empty() {
                return Err(anyhow!(
                    "番剧路径重设验证失败：无法为番剧 {} 生成有效的系列标题，这可能导致文件移动到错误位置",
                    video.name
                )
                .into());
            }

            // 验证生成的路径不包含明显的错误标识
            if series_title.contains("原版") || series_title.contains("中文") || series_title.contains("日语") {
                warn!(
                    "番剧路径重设警告：为番剧 {} 生成的系列标题 '{}' 包含版本标识，这可能不是预期的结果",
                    video.name, series_title
                );
            }

            info!("番剧路径重设验证通过：将使用系列标题 '{}'", series_title);
        }
    }

    Ok(())
}

/// 内部路径重设函数（用于队列处理和直接调用）
pub async fn reset_video_source_path_internal(
    db: Arc<DatabaseConnection>,
    source_type: String,
    id: i32,
    request: ResetVideoSourcePathRequest,
) -> Result<ResetVideoSourcePathResponse, ApiError> {
    // 使用主数据库连接

    // 在开始操作前进行安全验证
    let txn = db.begin().await?;
    validate_path_reset_safety(&txn, &source_type, id, &request.new_path).await?;
    let mut moved_files_count = 0;
    let mut updated_videos_count = 0;
    let mut cleaned_folders_count = 0;

    // 根据不同类型的视频源执行不同的路径重设操作
    let result = match source_type.as_str() {
        "collection" => {
            let collection = collection::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;
            let old_path = collection.path.clone();

            if request.apply_rename_rules {
                // 获取所有相关视频，按新路径规则移动文件
                let videos = video::Entity::find()
                    .filter(video::Column::CollectionId.eq(id))
                    .all(&txn)
                    .await?;

                for video in &videos {
                    // 移动视频文件到新路径结构
                    match move_video_files_to_new_path(video, &old_path, &request.new_path, request.clean_empty_folders)
                        .await
                    {
                        Ok((moved, cleaned)) => {
                            moved_files_count += moved;
                            cleaned_folders_count += cleaned;
                        }
                        Err(e) => warn!("移动视频 {} 文件失败: {}", video.id, e),
                    }

                    // 重新生成视频和分页的路径
                    if let Err(e) = regenerate_video_and_page_paths_correctly(&txn, video.id, &request.new_path).await {
                        warn!("更新视频 {} 路径失败: {:?}", video.id, e);
                    }
                }
                updated_videos_count = videos.len();
            }

            // 更新数据库中的路径
            collection::Entity::update_many()
                .filter(collection::Column::Id.eq(id))
                .col_expr(collection::Column::Path, Expr::value(request.new_path.clone()))
                .exec(&txn)
                .await?;

            ResetVideoSourcePathResponse {
                success: true,
                source_id: id,
                source_type: "collection".to_string(),
                old_path,
                new_path: request.new_path,
                moved_files_count,
                updated_videos_count,
                cleaned_folders_count,
                message: format!("合集 {} 路径重设完成", collection.name),
            }
        }
        "favorite" => {
            let favorite = favorite::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;
            let old_path = favorite.path.clone();

            if request.apply_rename_rules {
                // 获取所有相关视频，按新路径规则移动文件
                let videos = video::Entity::find()
                    .filter(video::Column::FavoriteId.eq(id))
                    .all(&txn)
                    .await?;

                for video in &videos {
                    // 移动视频文件到新路径结构
                    match move_video_files_to_new_path(video, &old_path, &request.new_path, request.clean_empty_folders)
                        .await
                    {
                        Ok((moved, cleaned)) => {
                            moved_files_count += moved;
                            cleaned_folders_count += cleaned;
                        }
                        Err(e) => warn!("移动视频 {} 文件失败: {}", video.id, e),
                    }

                    // 重新生成视频和分页的路径
                    if let Err(e) = regenerate_video_and_page_paths_correctly(&txn, video.id, &request.new_path).await {
                        warn!("更新视频 {} 路径失败: {:?}", video.id, e);
                    }
                }
                updated_videos_count = videos.len();
            }

            favorite::Entity::update_many()
                .filter(favorite::Column::Id.eq(id))
                .col_expr(favorite::Column::Path, Expr::value(request.new_path.clone()))
                .exec(&txn)
                .await?;

            ResetVideoSourcePathResponse {
                success: true,
                source_id: id,
                source_type: "favorite".to_string(),
                old_path,
                new_path: request.new_path,
                moved_files_count,
                updated_videos_count,
                cleaned_folders_count,
                message: format!("收藏夹 {} 路径重设完成", favorite.name),
            }
        }
        "submission" => {
            let submission = submission::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;
            let old_path = submission.path.clone();

            if request.apply_rename_rules {
                // 获取所有相关视频，按新路径规则移动文件
                let videos = video::Entity::find()
                    .filter(video::Column::SubmissionId.eq(id))
                    .all(&txn)
                    .await?;

                for video in &videos {
                    // 移动视频文件到新路径结构
                    match move_video_files_to_new_path(video, &old_path, &request.new_path, request.clean_empty_folders)
                        .await
                    {
                        Ok((moved, cleaned)) => {
                            moved_files_count += moved;
                            cleaned_folders_count += cleaned;
                        }
                        Err(e) => warn!("移动视频 {} 文件失败: {}", video.id, e),
                    }

                    // 重新生成视频和分页的路径
                    if let Err(e) = regenerate_video_and_page_paths_correctly(&txn, video.id, &request.new_path).await {
                        warn!("更新视频 {} 路径失败: {:?}", video.id, e);
                    }
                }
                updated_videos_count = videos.len();
            }

            submission::Entity::update_many()
                .filter(submission::Column::Id.eq(id))
                .col_expr(submission::Column::Path, Expr::value(request.new_path.clone()))
                .exec(&txn)
                .await?;

            ResetVideoSourcePathResponse {
                success: true,
                source_id: id,
                source_type: "submission".to_string(),
                old_path,
                new_path: request.new_path,
                moved_files_count,
                updated_videos_count,
                cleaned_folders_count,
                message: format!("UP主投稿 {} 路径重设完成", submission.upper_name),
            }
        }
        "watch_later" => {
            let watch_later = watch_later::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后再看"))?;
            let old_path = watch_later.path.clone();

            if request.apply_rename_rules {
                // 获取所有相关视频，按新路径规则移动文件
                let videos = video::Entity::find()
                    .filter(video::Column::WatchLaterId.eq(id))
                    .all(&txn)
                    .await?;

                for video in &videos {
                    // 移动视频文件到新路径结构
                    match move_video_files_to_new_path(video, &old_path, &request.new_path, request.clean_empty_folders)
                        .await
                    {
                        Ok((moved, cleaned)) => {
                            moved_files_count += moved;
                            cleaned_folders_count += cleaned;
                        }
                        Err(e) => warn!("移动视频 {} 文件失败: {}", video.id, e),
                    }

                    // 重新生成视频和分页的路径
                    if let Err(e) = regenerate_video_and_page_paths_correctly(&txn, video.id, &request.new_path).await {
                        warn!("更新视频 {} 路径失败: {:?}", video.id, e);
                    }
                }
                updated_videos_count = videos.len();
            }

            watch_later::Entity::update_many()
                .filter(watch_later::Column::Id.eq(id))
                .col_expr(watch_later::Column::Path, Expr::value(request.new_path.clone()))
                .exec(&txn)
                .await?;

            ResetVideoSourcePathResponse {
                success: true,
                source_id: id,
                source_type: "watch_later".to_string(),
                old_path,
                new_path: request.new_path,
                moved_files_count,
                updated_videos_count,
                cleaned_folders_count,
                message: "稍后再看路径重设完成".to_string(),
            }
        }
        "bangumi" => {
            let bangumi = video_source::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;
            let old_path = bangumi.path.clone();

            if request.apply_rename_rules {
                // 获取所有相关视频，按新路径规则移动文件
                let videos = video::Entity::find()
                    .filter(video::Column::SourceId.eq(id))
                    .filter(video::Column::SourceType.eq(1)) // 番剧类型
                    .all(&txn)
                    .await?;

                // 对于番剧，所有版本共享同一个文件夹，只需要移动一次
                if let Some(first_video) = videos.first() {
                    // 使用第一个视频来确定移动逻辑，只移动一次物理文件夹
                    match move_bangumi_files_to_new_path(
                        first_video,
                        &old_path,
                        &request.new_path,
                        request.clean_empty_folders,
                        &txn,
                    )
                    .await
                    {
                        Ok((moved, cleaned)) => {
                            moved_files_count += moved;
                            cleaned_folders_count += cleaned;

                            // 移动成功后，更新所有视频的数据库路径到相同的新路径
                            for video in &videos {
                                if let Err(e) =
                                    update_bangumi_video_path_in_database(&txn, video, &request.new_path).await
                                {
                                    warn!("更新番剧视频 {} 数据库路径失败: {:?}", video.id, e);
                                }
                            }
                        }
                        Err(e) => warn!("移动番剧文件夹失败: {}", e),
                    }
                }
                updated_videos_count = videos.len();
            }

            video_source::Entity::update_many()
                .filter(video_source::Column::Id.eq(id))
                .col_expr(video_source::Column::Path, Expr::value(request.new_path.clone()))
                .exec(&txn)
                .await?;

            ResetVideoSourcePathResponse {
                success: true,
                source_id: id,
                source_type: "bangumi".to_string(),
                old_path,
                new_path: request.new_path,
                moved_files_count,
                updated_videos_count,
                cleaned_folders_count,
                message: format!("番剧 {} 路径重设完成", bangumi.name),
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type).into()),
    };

    txn.commit().await?;
    Ok(result)
}

/// 使用四步重命名原则移动文件夹（直接移动到指定目标路径）
async fn move_files_with_four_step_rename(old_path: &str, target_path: &str) -> Result<String, std::io::Error> {
    use std::path::Path;

    let old_path = Path::new(old_path);
    let target_path = Path::new(target_path);

    if !old_path.exists() {
        return Ok(target_path.to_string_lossy().to_string()); // 如果原路径不存在，返回目标路径
    }

    // 如果目标路径已存在且和源路径相同，无需移动
    if old_path == target_path {
        return Ok(target_path.to_string_lossy().to_string());
    }

    // 确保目标目录的父目录存在
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // 四步重命名原则：
    // 1. 重命名到临时名称（在源目录下）
    let temp_name = format!(".temp_{}", crate::utils::time_format::beijing_now().timestamp_millis());
    let temp_path = old_path
        .parent()
        .ok_or_else(|| std::io::Error::other("无法获取父目录"))?
        .join(&temp_name);

    // 2. 移动到目标父目录（使用临时名称）
    let temp_target_path = target_path
        .parent()
        .ok_or_else(|| std::io::Error::other("无法获取目标父目录"))?
        .join(&temp_name);

    // 步骤1: 重命名到临时名称
    std::fs::rename(old_path, &temp_path)?;

    // 步骤2: 移动到目标目录
    std::fs::rename(&temp_path, &temp_target_path)?;

    // 步骤3: 重命名为最终名称
    let final_path = if target_path.exists() {
        // 如果目标已存在，使用冲突解决策略
        let mut counter = 1;
        let target_parent = target_path.parent().unwrap();
        let target_name = target_path.file_name().unwrap();

        loop {
            let conflict_name = format!("{}_{}", target_name.to_string_lossy(), counter);
            let conflict_path = target_parent.join(&conflict_name);
            if !conflict_path.exists() {
                std::fs::rename(&temp_target_path, &conflict_path)?;
                break conflict_path;
            }
            counter += 1;
        }
    } else {
        std::fs::rename(&temp_target_path, target_path)?;
        target_path.to_path_buf()
    };

    Ok(final_path.to_string_lossy().to_string())
}

/// 移动视频文件到新路径结构，返回(移动的文件数量, 清理的文件夹数量)
async fn move_video_files_to_new_path(
    video: &video::Model,
    _old_base_path: &str,
    new_base_path: &str,
    clean_empty_folders: bool,
) -> Result<(usize, usize), std::io::Error> {
    use std::path::Path;

    let mut moved_count = 0;
    let mut cleaned_count = 0;

    // 获取当前视频的存储路径
    let current_video_path = Path::new(&video.path);
    if !current_video_path.exists() {
        return Ok((0, 0)); // 如果视频文件夹不存在，跳过
    }

    // 使用模板重新生成视频在新基础路径下的目标路径
    let new_video_dir = Path::new(new_base_path);

    // 基于视频模型重新生成路径结构
    let new_video_path = crate::config::with_config(|bundle| {
        let video_args = crate::utils::format_arg::video_format_args(video);
        bundle.render_video_template(&video_args)
    })
    .map_err(|e| std::io::Error::other(format!("模板渲染失败: {}", e)))?;

    let target_video_dir = new_video_dir.join(&new_video_path);

    // 如果目标路径和当前路径相同，无需移动
    if current_video_path == target_video_dir {
        return Ok((0, 0));
    }

    // 使用四步重命名原则移动整个视频文件夹
    if (move_files_with_four_step_rename(
        &current_video_path.to_string_lossy(),
        &target_video_dir.to_string_lossy(),
    )
    .await)
        .is_ok()
    {
        moved_count = 1;

        // 移动成功后，检查并清理原来的父目录（如果启用了清理且为空）
        if clean_empty_folders {
            if let Some(parent_dir) = current_video_path.parent() {
                if let Ok(count) = cleanup_empty_directory(parent_dir).await {
                    cleaned_count = count;
                }
            }
        }
    }

    Ok((moved_count, cleaned_count))
}

/// 正确重新生成视频和分页路径（基于新的基础路径重新计算完整路径）
async fn regenerate_video_and_page_paths_correctly(
    txn: &sea_orm::DatabaseTransaction,
    video_id: i32,
    new_base_path: &str,
) -> Result<(), ApiError> {
    use std::path::Path;

    // 获取视频信息
    let video = video::Entity::find_by_id(video_id)
        .one(txn)
        .await?
        .ok_or_else(|| anyhow!("未找到视频记录"))?;

    // 重新生成视频路径
    let new_video_path = crate::config::with_config(|bundle| {
        let video_args = crate::utils::format_arg::video_format_args(&video);
        bundle.render_video_template(&video_args)
    })
    .map_err(|e| anyhow!("视频路径模板渲染失败: {}", e))?;

    let full_new_video_path = Path::new(new_base_path).join(&new_video_path);

    // 更新视频路径
    video::Entity::update_many()
        .filter(video::Column::Id.eq(video_id))
        .col_expr(
            video::Column::Path,
            Expr::value(full_new_video_path.to_string_lossy().to_string()),
        )
        .exec(txn)
        .await?;

    // 更新相关分页路径
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(video_id))
        .all(txn)
        .await?;

    for page_model in pages {
        // 重新生成分页路径
        let new_page_path = crate::config::with_config(|bundle| {
            let page_args = crate::utils::format_arg::page_format_args(&video, &page_model);
            bundle.render_page_template(&page_args)
        })
        .map_err(|e| anyhow!("分页路径模板渲染失败: {}", e))?;

        let full_new_page_path = full_new_video_path.join(format!("{}.mp4", new_page_path));

        page::Entity::update_many()
            .filter(page::Column::Id.eq(page_model.id))
            .col_expr(
                page::Column::Path,
                Expr::value(Some(full_new_page_path.to_string_lossy().to_string())),
            )
            .exec(txn)
            .await?;
    }

    Ok(())
}

/// 递归清理空目录（从指定目录开始向上清理）
async fn cleanup_empty_directory(dir_path: &std::path::Path) -> Result<usize, std::io::Error> {
    use tokio::fs;

    let mut cleaned_count = 0;
    let mut current_dir = dir_path;

    // 从当前目录开始，向上递归检查并清理空目录
    loop {
        if !current_dir.exists() {
            break;
        }

        // 检查目录是否为空
        let mut entries = fs::read_dir(current_dir).await?;
        if entries.next_entry().await?.is_none() {
            // 目录为空，可以删除
            match fs::remove_dir(current_dir).await {
                Ok(_) => {
                    cleaned_count += 1;
                    debug!("清理空目录: {}", current_dir.display());

                    // 继续检查父目录
                    if let Some(parent) = current_dir.parent() {
                        current_dir = parent;
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    debug!("清理目录失败 {}: {}", current_dir.display(), e);
                    break;
                }
            }
        } else {
            // 目录不为空，停止清理
            break;
        }
    }

    Ok(cleaned_count)
}

/// 获取当前配置
#[utoipa::path(
    get,
    path = "/api/config",
    responses(
        (status = 200, description = "获取配置成功", body = ConfigResponse),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn get_config() -> Result<ApiResponse<crate::api::response::ConfigResponse>, ApiError> {
    // 使用配置包系统获取最新配置
    let config = crate::config::with_config(|bundle| bundle.config.clone());

    let nfo_time_type = match config.nfo_time_type {
        crate::config::NFOTimeType::FavTime => "favtime",
        crate::config::NFOTimeType::PubTime => "pubtime",
    };

    Ok(ApiResponse::ok(crate::api::response::ConfigResponse {
        video_name: config.video_name.to_string(),
        page_name: config.page_name.to_string(),
        multi_page_name: config.multi_page_name.to_string(),
        bangumi_name: config.bangumi_name.to_string(),
        folder_structure: config.folder_structure.to_string(),
        bangumi_folder_name: config.bangumi_folder_name.to_string(),
        collection_folder_mode: config.collection_folder_mode.to_string(),
        collection_unified_name: config.collection_unified_name.to_string(),
        time_format: config.time_format.clone(),
        interval: config.interval,
        nfo_time_type: nfo_time_type.to_string(),
        parallel_download_enabled: config.concurrent_limit.parallel_download.enabled,
        parallel_download_threads: config.concurrent_limit.parallel_download.threads,
        parallel_download_use_aria2: config.concurrent_limit.parallel_download.use_aria2,
        // 视频质量设置
        video_max_quality: format!("{:?}", config.filter_option.video_max_quality),
        video_min_quality: format!("{:?}", config.filter_option.video_min_quality),
        audio_max_quality: format!("{:?}", config.filter_option.audio_max_quality),
        audio_min_quality: format!("{:?}", config.filter_option.audio_min_quality),
        codecs: config.filter_option.codecs.iter().map(|c| format!("{}", c)).collect(),
        no_dolby_video: config.filter_option.no_dolby_video,
        no_dolby_audio: config.filter_option.no_dolby_audio,
        no_hdr: config.filter_option.no_hdr,
        no_hires: config.filter_option.no_hires,
        // 弹幕设置
        danmaku_duration: config.danmaku_option.duration,
        danmaku_font: config.danmaku_option.font.clone(),
        danmaku_font_size: config.danmaku_option.font_size,
        danmaku_width_ratio: config.danmaku_option.width_ratio,
        danmaku_horizontal_gap: config.danmaku_option.horizontal_gap,
        danmaku_lane_size: config.danmaku_option.lane_size,
        danmaku_float_percentage: config.danmaku_option.float_percentage,
        danmaku_bottom_percentage: config.danmaku_option.bottom_percentage,
        danmaku_opacity: config.danmaku_option.opacity,
        danmaku_bold: config.danmaku_option.bold,
        danmaku_outline: config.danmaku_option.outline,
        danmaku_time_offset: config.danmaku_option.time_offset,
        // 并发控制设置
        concurrent_video: config.concurrent_limit.video,
        concurrent_page: config.concurrent_limit.page,
        rate_limit: config.concurrent_limit.rate_limit.as_ref().map(|r| r.limit),
        rate_duration: config.concurrent_limit.rate_limit.as_ref().map(|r| r.duration),
        // 其他设置
        cdn_sorting: config.cdn_sorting,
        // UP主投稿风控配置
        large_submission_threshold: config.submission_risk_control.large_submission_threshold,
        base_request_delay: config.submission_risk_control.base_request_delay,
        large_submission_delay_multiplier: config.submission_risk_control.large_submission_delay_multiplier,
        enable_progressive_delay: config.submission_risk_control.enable_progressive_delay,
        max_delay_multiplier: config.submission_risk_control.max_delay_multiplier,
        enable_incremental_fetch: config.submission_risk_control.enable_incremental_fetch,
        incremental_fallback_to_full: config.submission_risk_control.incremental_fallback_to_full,
        enable_batch_processing: config.submission_risk_control.enable_batch_processing,
        batch_size: config.submission_risk_control.batch_size,
        batch_delay_seconds: config.submission_risk_control.batch_delay_seconds,
        enable_auto_backoff: config.submission_risk_control.enable_auto_backoff,
        auto_backoff_base_seconds: config.submission_risk_control.auto_backoff_base_seconds,
        auto_backoff_max_multiplier: config.submission_risk_control.auto_backoff_max_multiplier,
        source_delay_seconds: config.submission_risk_control.source_delay_seconds,
        submission_source_delay_seconds: config.submission_risk_control.submission_source_delay_seconds,
        // UP主投稿源扫描策略
        submission_scan_batch_size: config.submission_scan_strategy.batch_size,
        submission_adaptive_scan: config.submission_scan_strategy.adaptive_enabled,
        submission_adaptive_max_hours: config.submission_scan_strategy.adaptive_max_hours,
        scan_deleted_videos: config.scan_deleted_videos,
        // aria2监控配置
        enable_aria2_health_check: config.enable_aria2_health_check,
        enable_aria2_auto_restart: config.enable_aria2_auto_restart,
        aria2_health_check_interval: config.aria2_health_check_interval,
        // 多P视频目录结构配置
        multi_page_use_season_structure: config.multi_page_use_season_structure,
        // 合集目录结构配置
        collection_use_season_structure: config.collection_use_season_structure,
        // 番剧目录结构配置
        bangumi_use_season_structure: config.bangumi_use_season_structure,
        // UP主头像保存路径
        upper_path: config.upper_path.to_string_lossy().to_string(),
        // ffmpeg 路径
        ffmpeg_path: config.ffmpeg_path.clone(),
        // B站凭证信息
        credential: {
            let credential = config.credential.load();
            credential.as_deref().map(|cred| crate::api::response::CredentialInfo {
                sessdata: cred.sessdata.clone(),
                bili_jct: cred.bili_jct.clone(),
                buvid3: cred.buvid3.clone(),
                dedeuserid: cred.dedeuserid.clone(),
                ac_time_value: cred.ac_time_value.clone(),
                buvid4: cred.buvid4.clone(),
                dedeuserid_ckmd5: cred.dedeuserid_ckmd5.clone(),
            })
        },
        // 推送通知配置
        notification: crate::api::response::NotificationConfigResponse {
            active_channel: config.notification.active_channel.clone(),
            serverchan_key: config.notification.serverchan_key.clone(),
            serverchan3_uid: config.notification.serverchan3_uid.clone(),
            serverchan3_sendkey: config.notification.serverchan3_sendkey.clone(),
            wecom_webhook_url: config.notification.wecom_webhook_url.clone(),
            wecom_msgtype: config.notification.wecom_msgtype.clone(),
            wecom_mention_all: config.notification.wecom_mention_all,
            wecom_mentioned_list: config.notification.wecom_mentioned_list.clone(),
            enable_scan_notifications: config.notification.enable_scan_notifications,
            notification_min_videos: config.notification.notification_min_videos,
            notification_timeout: config.notification.notification_timeout,
            notification_retry_count: config.notification.notification_retry_count,
        },
        // 风控验证配置
        risk_control: crate::api::response::RiskControlConfigResponse {
            enabled: config.risk_control.enabled,
            mode: config.risk_control.mode.clone(),
            timeout: config.risk_control.timeout,
            auto_solve: config.risk_control.auto_solve.as_ref().map(|auto_solve| {
                crate::api::response::AutoSolveConfigResponse {
                    service: auto_solve.service.clone(),
                    api_key: auto_solve.api_key.clone(),
                    max_retries: auto_solve.max_retries,
                    solve_timeout: auto_solve.solve_timeout,
                }
            }),
        },
        // AI重命名配置
        ai_rename: crate::api::response::AiRenameConfigResponse {
            enabled: config.ai_rename.enabled,
            provider: config.ai_rename.provider.clone(),
            base_url: config.ai_rename.base_url.clone(),
            api_key: config.ai_rename.api_key.clone(),
            deepseek_web_token: config.ai_rename.deepseek_web_token.clone(),
            model: config.ai_rename.model.clone(),
            timeout_seconds: config.ai_rename.timeout_seconds,
            video_prompt_hint: config.ai_rename.video_prompt_hint.clone(),
            audio_prompt_hint: config.ai_rename.audio_prompt_hint.clone(),
            rename_parent_dir: config.ai_rename.rename_parent_dir,
        },
        // 服务器绑定地址
        bind_address: config.bind_address.clone(),
    }))
}

/// 更新配置
#[utoipa::path(
    put,
    path = "/api/config",
    request_body = UpdateConfigRequest,
    responses(
        (status = 200, description = "配置更新成功", body = UpdateConfigResponse),
        (status = 400, description = "请求参数错误", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn update_config(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(params): axum::Json<crate::api::request::UpdateConfigRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateConfigResponse>, ApiError> {
    // 检查是否正在扫描
    if crate::task::is_scanning() {
        // 正在扫描，将更新配置任务加入队列
        let task_id = uuid::Uuid::new_v4().to_string();
        let update_task = crate::task::UpdateConfigTask {
            video_name: params.video_name.clone(),
            page_name: params.page_name.clone(),
            multi_page_name: params.multi_page_name.clone(),
            bangumi_name: params.bangumi_name.clone(),
            folder_structure: params.folder_structure.clone(),
            bangumi_folder_name: params.bangumi_folder_name.clone(),
            collection_folder_mode: params.collection_folder_mode.clone(),
            collection_unified_name: params.collection_unified_name.clone(),
            time_format: params.time_format.clone(),
            interval: params.interval,
            nfo_time_type: params.nfo_time_type.clone(),
            parallel_download_enabled: params.parallel_download_enabled,
            parallel_download_threads: params.parallel_download_threads,
            parallel_download_use_aria2: params.parallel_download_use_aria2,
            // 视频质量设置
            video_max_quality: params.video_max_quality.clone(),
            video_min_quality: params.video_min_quality.clone(),
            audio_max_quality: params.audio_max_quality.clone(),
            audio_min_quality: params.audio_min_quality.clone(),
            codecs: params.codecs.clone(),
            no_dolby_video: params.no_dolby_video,
            no_dolby_audio: params.no_dolby_audio,
            no_hdr: params.no_hdr,
            no_hires: params.no_hires,
            // 弹幕设置
            danmaku_duration: params.danmaku_duration,
            danmaku_font: params.danmaku_font.clone(),
            danmaku_font_size: params.danmaku_font_size,
            danmaku_width_ratio: params.danmaku_width_ratio,
            danmaku_horizontal_gap: params.danmaku_horizontal_gap,
            danmaku_lane_size: params.danmaku_lane_size,
            danmaku_float_percentage: params.danmaku_float_percentage,
            danmaku_bottom_percentage: params.danmaku_bottom_percentage,
            danmaku_opacity: params.danmaku_opacity,
            danmaku_bold: params.danmaku_bold,
            danmaku_outline: params.danmaku_outline,
            danmaku_time_offset: params.danmaku_time_offset,
            // 并发控制设置
            concurrent_video: params.concurrent_video,
            concurrent_page: params.concurrent_page,
            rate_limit: params.rate_limit,
            rate_duration: params.rate_duration,
            // 其他设置
            cdn_sorting: params.cdn_sorting,
            // UP主投稿风控配置
            large_submission_threshold: params.large_submission_threshold,
            base_request_delay: params.base_request_delay,
            large_submission_delay_multiplier: params.large_submission_delay_multiplier,
            enable_progressive_delay: params.enable_progressive_delay,
            max_delay_multiplier: params.max_delay_multiplier,
            enable_incremental_fetch: params.enable_incremental_fetch,
            incremental_fallback_to_full: params.incremental_fallback_to_full,
            enable_batch_processing: params.enable_batch_processing,
            batch_size: params.batch_size,
            batch_delay_seconds: params.batch_delay_seconds,
            enable_auto_backoff: params.enable_auto_backoff,
            auto_backoff_base_seconds: params.auto_backoff_base_seconds,
            auto_backoff_max_multiplier: params.auto_backoff_max_multiplier,
            source_delay_seconds: params.source_delay_seconds,
            submission_source_delay_seconds: params.submission_source_delay_seconds,
            // UP主投稿源扫描策略
            submission_scan_batch_size: params.submission_scan_batch_size,
            submission_adaptive_scan: params.submission_adaptive_scan,
            submission_adaptive_max_hours: params.submission_adaptive_max_hours,
            // 多P视频目录结构配置
            multi_page_use_season_structure: params.multi_page_use_season_structure,
            // 合集目录结构配置
            collection_use_season_structure: params.collection_use_season_structure,
            // 番剧目录结构配置
            bangumi_use_season_structure: params.bangumi_use_season_structure,
            // UP主头像保存路径
            upper_path: params.upper_path.clone(),
            // ffmpeg 路径
            ffmpeg_path: params.ffmpeg_path.clone(),
            ai_rename_rename_parent_dir: params.ai_rename_rename_parent_dir,
            task_id: task_id.clone(),
        };

        crate::task::enqueue_update_task(update_task, &db).await?;

        info!("检测到正在扫描，更新配置任务已加入队列等待处理");

        return Ok(ApiResponse::ok(crate::api::response::UpdateConfigResponse {
            success: true,
            message: "正在扫描中，更新配置任务已加入队列，将在扫描完成后自动处理".to_string(),
            updated_files: None,
            resetted_nfo_videos_count: None,
            resetted_nfo_pages_count: None,
        }));
    }

    // 没有扫描，直接执行更新配置
    match update_config_internal(db, params).await {
        Ok(response) => Ok(ApiResponse::ok(response)),
        Err(e) => Err(e),
    }
}

/// 内部更新配置函数（用于队列处理和直接调用）
fn config_update_field_display_name(field: &str) -> String {
    let known = match field {
        "video_name" => Some("视频命名模板"),
        "page_name" => Some("单P分页命名模板"),
        "multi_page_name" => Some("多P分页命名模板"),
        "bangumi_name" => Some("番剧分页命名模板"),
        "folder_structure" => Some("目录结构模板"),
        "bangumi_folder_name" => Some("番剧文件夹命名模板"),
        "collection_folder_mode" => Some("合集文件夹模式"),
        "collection_unified_name" => Some("合集统一命名模板"),
        "time_format" => Some("时间格式"),
        "interval" => Some("扫描间隔"),
        "nfo_time_type" => Some("NFO时间类型"),
        "parallel_download_enabled" => Some("多线程下载开关"),
        "parallel_download_threads" => Some("下载线程数"),
        "parallel_download_use_aria2" => Some("优先使用aria2"),
        "video_max_quality" => Some("视频最高画质"),
        "video_min_quality" => Some("视频最低画质"),
        "audio_max_quality" => Some("音频最高音质"),
        "audio_min_quality" => Some("音频最低音质"),
        "codecs" => Some("视频编码偏好"),
        "no_dolby_video" => Some("禁用杜比视界"),
        "no_dolby_audio" => Some("禁用杜比全景声"),
        "no_hdr" => Some("禁用HDR"),
        "no_hires" => Some("禁用Hi-Res"),
        "danmaku_duration" => Some("弹幕持续时间"),
        "danmaku_font" => Some("弹幕字体"),
        "danmaku_font_size" => Some("弹幕字号"),
        "danmaku_width_ratio" => Some("弹幕宽度比例"),
        "danmaku_horizontal_gap" => Some("弹幕水平间距"),
        "danmaku_lane_size" => Some("弹幕轨道高度"),
        "danmaku_float_percentage" => Some("滚动弹幕占比"),
        "danmaku_bottom_percentage" => Some("底部弹幕占比"),
        "danmaku_opacity" => Some("弹幕透明度"),
        "danmaku_bold" => Some("弹幕加粗"),
        "danmaku_outline" => Some("弹幕描边"),
        "danmaku_time_offset" => Some("弹幕时间偏移"),
        "concurrent_video" => Some("同时处理视频数"),
        "concurrent_page" => Some("每视频并发分页数"),
        "rate_limit" => Some("请求频率限制"),
        "rate_duration" => Some("请求时间窗口"),
        "cdn_sorting" => Some("CDN优先级排序"),
        "scan_deleted_videos" => Some("扫描已删除视频"),
        "enable_aria2_health_check" => Some("aria2健康检查开关"),
        "enable_aria2_auto_restart" => Some("aria2自动重启开关"),
        "aria2_health_check_interval" => Some("aria2健康检查间隔"),
        "large_submission_threshold" => Some("大投稿判定阈值"),
        "base_request_delay" => Some("基础请求延迟"),
        "large_submission_delay_multiplier" => Some("大投稿延迟倍率"),
        "enable_progressive_delay" => Some("渐进延迟开关"),
        "max_delay_multiplier" => Some("最大延迟倍率"),
        "enable_incremental_fetch" => Some("增量抓取开关"),
        "incremental_fallback_to_full" => Some("增量失败回退全量"),
        "enable_batch_processing" => Some("分批处理开关"),
        "batch_size" => Some("分批大小"),
        "batch_delay_seconds" => Some("分批延迟"),
        "enable_auto_backoff" => Some("自动退避开关"),
        "auto_backoff_base_seconds" => Some("自动退避基础时长"),
        "auto_backoff_max_multiplier" => Some("自动退避最大倍率"),
        "source_delay_seconds" => Some("视频源切换延迟"),
        "submission_source_delay_seconds" => Some("投稿源切换延迟"),
        "submission_scan_batch_size" => Some("投稿每轮扫描上限"),
        "submission_adaptive_scan" => Some("投稿自适应扫描开关"),
        "submission_adaptive_max_hours" => Some("投稿自适应最大间隔"),
        "multi_page_use_season_structure" => Some("多P使用Season目录"),
        "collection_use_season_structure" => Some("合集使用Season目录"),
        "bangumi_use_season_structure" => Some("番剧使用Season目录"),
        "upper_path" => Some("UP头像缓存路径"),
        "ffmpeg_path" => Some("ffmpeg路径"),
        "bind_address" => Some("服务监听地址"),
        "risk_control.enabled" => Some("风控验证开关"),
        "risk_control.mode" => Some("风控验证模式"),
        "risk_control.timeout" => Some("风控验证超时"),
        "risk_control.auto_solve.service" => Some("自动打码服务"),
        "risk_control.auto_solve.api_key" => Some("自动打码密钥"),
        "risk_control.auto_solve.max_retries" => Some("自动打码最大重试"),
        "risk_control.auto_solve.solve_timeout" => Some("自动打码单次超时"),
        "ai_rename" => Some("AI重命名配置"),
        _ => None,
    };

    if let Some(name) = known {
        return name.to_string();
    }

    let fallback = crate::config::describe_config_key(field);
    if fallback == "未知/未定义" {
        field.to_string()
    } else {
        fallback.to_string()
    }
}

fn format_config_update_fields_display(updated_fields: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for field in updated_fields {
        let name = config_update_field_display_name(field);
        if seen.insert(name.clone()) {
            result.push(name);
        }
    }

    result
}

pub async fn update_config_internal(
    db: Arc<DatabaseConnection>,
    params: crate::api::request::UpdateConfigRequest,
) -> Result<crate::api::response::UpdateConfigResponse, ApiError> {
    use std::borrow::Cow;

    fn has_path_separator_outside_handlebars(template: &str) -> bool {
        let mut i = 0usize;
        let mut in_tag = false;
        let mut tag_end_len = 0usize;

        while i < template.len() {
            let bytes = template.as_bytes();

            if !in_tag {
                // 进入 Handlebars 标签（{{ / {{{ / {{{{）
                if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    let mut start_len = 2usize;
                    while i + start_len < bytes.len() && bytes[i + start_len] == b'{' && start_len < 4 {
                        start_len += 1;
                    }
                    in_tag = true;
                    tag_end_len = start_len;
                    i += start_len;
                    continue;
                }

                // 标签外：任意 / 或 \\ 都视为路径分隔符
                if bytes[i] == b'/' || bytes[i] == b'\\' {
                    return true;
                }

                let ch = template[i..].chars().next().unwrap();
                i += ch.len_utf8();
                continue;
            }

            // Handlebars 标签内：寻找结束符（}} / }}} / }}}}）
            if bytes[i] == b'}' && tag_end_len > 0 {
                let mut ok = true;
                for k in 0..tag_end_len {
                    if i + k >= bytes.len() || bytes[i + k] != b'}' {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    i += tag_end_len;
                    in_tag = false;
                    tag_end_len = 0;
                    continue;
                }
            }

            let ch = template[i..].chars().next().unwrap();
            i += ch.len_utf8();
        }

        false
    }

    // 获取当前配置的副本
    let mut config = crate::config::reload_config();
    let mut updated_fields = Vec::new();

    // 记录原始的NFO时间类型，用于比较是否真正发生了变化
    let original_nfo_time_type = config.nfo_time_type.clone();

    // 记录原始的命名相关配置，用于比较是否真正发生了变化
    let original_video_name = config.video_name.clone();
    let original_page_name = config.page_name.clone();
    let original_multi_page_name = config.multi_page_name.clone();
    let original_bangumi_name = config.bangumi_name.clone();
    let original_folder_structure = config.folder_structure.clone();
    let original_collection_folder_mode = config.collection_folder_mode.clone();
    let original_collection_unified_name = config.collection_unified_name.clone();

    // 更新配置字段
    if let Some(video_name) = params.video_name {
        if !video_name.trim().is_empty() && video_name != original_video_name.as_ref() {
            config.video_name = Cow::Owned(video_name);
            updated_fields.push("video_name");
        }
    }

    if let Some(page_name) = params.page_name {
        if !page_name.trim().is_empty() && page_name != original_page_name.as_ref() {
            config.page_name = Cow::Owned(page_name);
            updated_fields.push("page_name");
        }
    }

    if let Some(multi_page_name) = params.multi_page_name {
        if !multi_page_name.trim().is_empty() && multi_page_name != original_multi_page_name.as_ref() {
            config.multi_page_name = Cow::Owned(multi_page_name);
            updated_fields.push("multi_page_name");
        }
    }

    if let Some(folder_structure) = params.folder_structure {
        if !folder_structure.trim().is_empty() && folder_structure != original_folder_structure.as_ref() {
            config.folder_structure = Cow::Owned(folder_structure);
            updated_fields.push("folder_structure");
        }
    }

    if let Some(collection_folder_mode) = params.collection_folder_mode {
        if !collection_folder_mode.trim().is_empty()
            && collection_folder_mode != original_collection_folder_mode.as_ref()
        {
            // 验证合集文件夹模式的有效性
            match collection_folder_mode.as_str() {
                "separate" | "unified" | "up_seasonal" => {
                    config.collection_folder_mode = Cow::Owned(collection_folder_mode);
                    updated_fields.push("collection_folder_mode");

                    // 同UP合集分季模式依赖Season目录结构，自动开启避免配置冲突
                    if config.collection_folder_mode.as_ref() == "up_seasonal"
                        && !config.collection_use_season_structure
                    {
                        config.collection_use_season_structure = true;
                        if !updated_fields.contains(&"collection_use_season_structure") {
                            updated_fields.push("collection_use_season_structure");
                        }
                    }
                }
                _ => {
                    return Err(
                        anyhow!(
                            "无效的合集文件夹模式，只支持 'separate'（分离模式）、'unified'（统一模式）或 'up_seasonal'（同UP合集分季）"
                        )
                        .into(),
                    )
                }
            }
        }
    }

    if let Some(collection_unified_name) = params.collection_unified_name {
        let trimmed = collection_unified_name.trim();
        if has_path_separator_outside_handlebars(trimmed) {
            return Err(anyhow!("合集统一模式命名模板不应包含路径分隔符 / 或 \\").into());
        }
        if !trimmed.is_empty() && trimmed != original_collection_unified_name.as_ref() {
            config.collection_unified_name = Cow::Owned(trimmed.to_string());
            updated_fields.push("collection_unified_name");
        }
    }

    if let Some(time_format) = params.time_format {
        if !time_format.trim().is_empty() && time_format != config.time_format {
            config.time_format = time_format;
            updated_fields.push("time_format");
        }
    }

    if let Some(interval) = params.interval {
        if interval > 0 && interval != config.interval {
            config.interval = interval;
            updated_fields.push("interval");
        }
    }

    if let Some(nfo_time_type) = params.nfo_time_type {
        let new_nfo_time_type = match nfo_time_type.as_str() {
            "favtime" => crate::config::NFOTimeType::FavTime,
            "pubtime" => crate::config::NFOTimeType::PubTime,
            _ => return Err(anyhow!("无效的NFO时间类型，只支持 'favtime' 或 'pubtime'").into()),
        };

        // 只有当NFO时间类型真正发生变化时才标记为需要更新
        if original_nfo_time_type != new_nfo_time_type {
            config.nfo_time_type = new_nfo_time_type;
            updated_fields.push("nfo_time_type");
        }
    }

    if let Some(bangumi_name) = params.bangumi_name {
        if !bangumi_name.trim().is_empty() && bangumi_name != original_bangumi_name.as_ref() {
            config.bangumi_name = Cow::Owned(bangumi_name);
            updated_fields.push("bangumi_name");
        }
    }

    if let Some(bangumi_folder_name) = params.bangumi_folder_name {
        if !bangumi_folder_name.trim().is_empty() && bangumi_folder_name != config.bangumi_folder_name.as_ref() {
            config.bangumi_folder_name = Cow::Owned(bangumi_folder_name);
            updated_fields.push("bangumi_folder_name");
        }
    }

    // 处理多线程下载配置
    if let Some(enabled) = params.parallel_download_enabled {
        if enabled != config.concurrent_limit.parallel_download.enabled {
            config.concurrent_limit.parallel_download.enabled = enabled;
            updated_fields.push("parallel_download_enabled");
        }
    }

    if let Some(threads) = params.parallel_download_threads {
        if threads > 0 && threads != config.concurrent_limit.parallel_download.threads {
            config.concurrent_limit.parallel_download.threads = threads;
            updated_fields.push("parallel_download_threads");
        }
    }

    if let Some(use_aria2) = params.parallel_download_use_aria2 {
        if use_aria2 != config.concurrent_limit.parallel_download.use_aria2 {
            config.concurrent_limit.parallel_download.use_aria2 = use_aria2;
            updated_fields.push("parallel_download_use_aria2");
        }
    }

    // 处理视频质量设置
    if let Some(quality) = params.video_max_quality {
        use crate::bilibili::VideoQuality;
        if let Ok(new_quality) = quality.parse::<VideoQuality>() {
            if new_quality != config.filter_option.video_max_quality {
                config.filter_option.video_max_quality = new_quality;
                updated_fields.push("video_max_quality");
            }
        }
    }

    if let Some(quality) = params.video_min_quality {
        use crate::bilibili::VideoQuality;
        if let Ok(new_quality) = quality.parse::<VideoQuality>() {
            if new_quality != config.filter_option.video_min_quality {
                config.filter_option.video_min_quality = new_quality;
                updated_fields.push("video_min_quality");
            }
        }
    }

    if let Some(quality) = params.audio_max_quality {
        use crate::bilibili::AudioQuality;
        if let Ok(new_quality) = quality.parse::<AudioQuality>() {
            if new_quality != config.filter_option.audio_max_quality {
                config.filter_option.audio_max_quality = new_quality;
                updated_fields.push("audio_max_quality");
            }
        }
    }

    if let Some(quality) = params.audio_min_quality {
        use crate::bilibili::AudioQuality;
        if let Ok(new_quality) = quality.parse::<AudioQuality>() {
            if new_quality != config.filter_option.audio_min_quality {
                config.filter_option.audio_min_quality = new_quality;
                updated_fields.push("audio_min_quality");
            }
        }
    }

    if let Some(codecs) = params.codecs {
        use crate::bilibili::VideoCodecs;
        let mut new_codecs = Vec::new();
        for codec_str in codecs {
            if let Ok(codec) = codec_str.parse::<VideoCodecs>() {
                new_codecs.push(codec);
            }
        }
        if !new_codecs.is_empty() && new_codecs != config.filter_option.codecs {
            config.filter_option.codecs = new_codecs;
            updated_fields.push("codecs");
        }
    }

    if let Some(no_dolby_video) = params.no_dolby_video {
        if no_dolby_video != config.filter_option.no_dolby_video {
            config.filter_option.no_dolby_video = no_dolby_video;
            updated_fields.push("no_dolby_video");
        }
    }

    if let Some(no_dolby_audio) = params.no_dolby_audio {
        if no_dolby_audio != config.filter_option.no_dolby_audio {
            config.filter_option.no_dolby_audio = no_dolby_audio;
            updated_fields.push("no_dolby_audio");
        }
    }

    if let Some(no_hdr) = params.no_hdr {
        if no_hdr != config.filter_option.no_hdr {
            config.filter_option.no_hdr = no_hdr;
            updated_fields.push("no_hdr");
        }
    }

    if let Some(no_hires) = params.no_hires {
        if no_hires != config.filter_option.no_hires {
            config.filter_option.no_hires = no_hires;
            updated_fields.push("no_hires");
        }
    }

    // 处理弹幕设置
    if let Some(duration) = params.danmaku_duration {
        if duration != config.danmaku_option.duration {
            config.danmaku_option.duration = duration;
            updated_fields.push("danmaku_duration");
        }
    }

    if let Some(font) = params.danmaku_font {
        if !font.trim().is_empty() && font != config.danmaku_option.font {
            config.danmaku_option.font = font;
            updated_fields.push("danmaku_font");
        }
    }

    if let Some(font_size) = params.danmaku_font_size {
        if font_size != config.danmaku_option.font_size {
            config.danmaku_option.font_size = font_size;
            updated_fields.push("danmaku_font_size");
        }
    }

    if let Some(width_ratio) = params.danmaku_width_ratio {
        if width_ratio != config.danmaku_option.width_ratio {
            config.danmaku_option.width_ratio = width_ratio;
            updated_fields.push("danmaku_width_ratio");
        }
    }

    if let Some(horizontal_gap) = params.danmaku_horizontal_gap {
        if horizontal_gap != config.danmaku_option.horizontal_gap {
            config.danmaku_option.horizontal_gap = horizontal_gap;
            updated_fields.push("danmaku_horizontal_gap");
        }
    }

    if let Some(lane_size) = params.danmaku_lane_size {
        if lane_size != config.danmaku_option.lane_size {
            config.danmaku_option.lane_size = lane_size;
            updated_fields.push("danmaku_lane_size");
        }
    }

    if let Some(float_percentage) = params.danmaku_float_percentage {
        if float_percentage != config.danmaku_option.float_percentage {
            config.danmaku_option.float_percentage = float_percentage;
            updated_fields.push("danmaku_float_percentage");
        }
    }

    if let Some(bottom_percentage) = params.danmaku_bottom_percentage {
        if bottom_percentage != config.danmaku_option.bottom_percentage {
            config.danmaku_option.bottom_percentage = bottom_percentage;
            updated_fields.push("danmaku_bottom_percentage");
        }
    }

    if let Some(opacity) = params.danmaku_opacity {
        if opacity != config.danmaku_option.opacity {
            config.danmaku_option.opacity = opacity;
            updated_fields.push("danmaku_opacity");
        }
    }

    if let Some(bold) = params.danmaku_bold {
        if bold != config.danmaku_option.bold {
            config.danmaku_option.bold = bold;
            updated_fields.push("danmaku_bold");
        }
    }

    if let Some(outline) = params.danmaku_outline {
        if outline != config.danmaku_option.outline {
            config.danmaku_option.outline = outline;
            updated_fields.push("danmaku_outline");
        }
    }

    if let Some(time_offset) = params.danmaku_time_offset {
        if time_offset != config.danmaku_option.time_offset {
            config.danmaku_option.time_offset = time_offset;
            updated_fields.push("danmaku_time_offset");
        }
    }

    // 处理并发控制设置
    if let Some(concurrent_video) = params.concurrent_video {
        if concurrent_video > 0 && concurrent_video != config.concurrent_limit.video {
            config.concurrent_limit.video = concurrent_video;
            updated_fields.push("concurrent_video");
        }
    }

    if let Some(concurrent_page) = params.concurrent_page {
        if concurrent_page > 0 && concurrent_page != config.concurrent_limit.page {
            config.concurrent_limit.page = concurrent_page;
            updated_fields.push("concurrent_page");
        }
    }

    if let Some(rate_limit) = params.rate_limit {
        if rate_limit > 0 {
            let current_limit = config
                .concurrent_limit
                .rate_limit
                .as_ref()
                .map(|r| r.limit)
                .unwrap_or(0);
            if rate_limit != current_limit {
                if let Some(ref mut rate) = config.concurrent_limit.rate_limit {
                    rate.limit = rate_limit;
                } else {
                    config.concurrent_limit.rate_limit = Some(crate::config::RateLimit {
                        limit: rate_limit,
                        duration: 250, // 默认值
                    });
                }
                updated_fields.push("rate_limit");
            }
        }
    }

    if let Some(rate_duration) = params.rate_duration {
        if rate_duration > 0 {
            let current_duration = config
                .concurrent_limit
                .rate_limit
                .as_ref()
                .map(|r| r.duration)
                .unwrap_or(0);
            if rate_duration != current_duration {
                if let Some(ref mut rate) = config.concurrent_limit.rate_limit {
                    rate.duration = rate_duration;
                } else {
                    config.concurrent_limit.rate_limit = Some(crate::config::RateLimit {
                        limit: 4, // 默认值
                        duration: rate_duration,
                    });
                }
                updated_fields.push("rate_duration");
            }
        }
    }

    // 处理其他设置
    if let Some(cdn_sorting) = params.cdn_sorting {
        if cdn_sorting != config.cdn_sorting {
            config.cdn_sorting = cdn_sorting;
            updated_fields.push("cdn_sorting");
        }
    }

    // 处理显示已删除视频配置
    if let Some(scan_deleted) = params.scan_deleted_videos {
        if scan_deleted != config.scan_deleted_videos {
            config.scan_deleted_videos = scan_deleted;
            updated_fields.push("scan_deleted_videos");
        }
    }

    // 处理aria2监控配置
    if let Some(enable_health_check) = params.enable_aria2_health_check {
        if enable_health_check != config.enable_aria2_health_check {
            config.enable_aria2_health_check = enable_health_check;
            updated_fields.push("enable_aria2_health_check");
        }
    }

    if let Some(enable_auto_restart) = params.enable_aria2_auto_restart {
        if enable_auto_restart != config.enable_aria2_auto_restart {
            config.enable_aria2_auto_restart = enable_auto_restart;
            updated_fields.push("enable_aria2_auto_restart");
        }
    }

    if let Some(check_interval) = params.aria2_health_check_interval {
        if check_interval > 0 && check_interval != config.aria2_health_check_interval {
            config.aria2_health_check_interval = check_interval;
            updated_fields.push("aria2_health_check_interval");
        }
    }

    // 处理UP主投稿风控配置
    if let Some(threshold) = params.large_submission_threshold {
        if threshold != config.submission_risk_control.large_submission_threshold {
            config.submission_risk_control.large_submission_threshold = threshold;
            updated_fields.push("large_submission_threshold");
        }
    }

    if let Some(delay) = params.base_request_delay {
        if delay != config.submission_risk_control.base_request_delay {
            config.submission_risk_control.base_request_delay = delay;
            updated_fields.push("base_request_delay");
        }
    }

    if let Some(multiplier) = params.large_submission_delay_multiplier {
        if multiplier != config.submission_risk_control.large_submission_delay_multiplier {
            config.submission_risk_control.large_submission_delay_multiplier = multiplier;
            updated_fields.push("large_submission_delay_multiplier");
        }
    }

    if let Some(enabled) = params.enable_progressive_delay {
        if enabled != config.submission_risk_control.enable_progressive_delay {
            config.submission_risk_control.enable_progressive_delay = enabled;
            updated_fields.push("enable_progressive_delay");
        }
    }

    if let Some(multiplier) = params.max_delay_multiplier {
        if multiplier != config.submission_risk_control.max_delay_multiplier {
            config.submission_risk_control.max_delay_multiplier = multiplier;
            updated_fields.push("max_delay_multiplier");
        }
    }

    if let Some(enabled) = params.enable_incremental_fetch {
        if enabled != config.submission_risk_control.enable_incremental_fetch {
            config.submission_risk_control.enable_incremental_fetch = enabled;
            updated_fields.push("enable_incremental_fetch");
        }
    }

    if let Some(enabled) = params.incremental_fallback_to_full {
        if enabled != config.submission_risk_control.incremental_fallback_to_full {
            config.submission_risk_control.incremental_fallback_to_full = enabled;
            updated_fields.push("incremental_fallback_to_full");
        }
    }

    if let Some(enabled) = params.enable_batch_processing {
        if enabled != config.submission_risk_control.enable_batch_processing {
            config.submission_risk_control.enable_batch_processing = enabled;
            updated_fields.push("enable_batch_processing");
        }
    }

    if let Some(size) = params.batch_size {
        if size != config.submission_risk_control.batch_size {
            config.submission_risk_control.batch_size = size;
            updated_fields.push("batch_size");
        }
    }

    if let Some(delay) = params.batch_delay_seconds {
        if delay != config.submission_risk_control.batch_delay_seconds {
            config.submission_risk_control.batch_delay_seconds = delay;
            updated_fields.push("batch_delay_seconds");
        }
    }

    if let Some(enabled) = params.enable_auto_backoff {
        if enabled != config.submission_risk_control.enable_auto_backoff {
            config.submission_risk_control.enable_auto_backoff = enabled;
            updated_fields.push("enable_auto_backoff");
        }
    }

    if let Some(seconds) = params.auto_backoff_base_seconds {
        if seconds != config.submission_risk_control.auto_backoff_base_seconds {
            config.submission_risk_control.auto_backoff_base_seconds = seconds;
            updated_fields.push("auto_backoff_base_seconds");
        }
    }

    if let Some(multiplier) = params.auto_backoff_max_multiplier {
        if multiplier != config.submission_risk_control.auto_backoff_max_multiplier {
            config.submission_risk_control.auto_backoff_max_multiplier = multiplier;
            updated_fields.push("auto_backoff_max_multiplier");
        }
    }

    // 处理视频源间延迟配置
    if let Some(delay) = params.source_delay_seconds {
        if delay != config.submission_risk_control.source_delay_seconds {
            config.submission_risk_control.source_delay_seconds = delay;
            updated_fields.push("source_delay_seconds");
        }
    }

    if let Some(delay) = params.submission_source_delay_seconds {
        if delay != config.submission_risk_control.submission_source_delay_seconds {
            config.submission_risk_control.submission_source_delay_seconds = delay;
            updated_fields.push("submission_source_delay_seconds");
        }
    }

    // UP主投稿源扫描策略
    if let Some(size) = params.submission_scan_batch_size {
        if size != config.submission_scan_strategy.batch_size {
            config.submission_scan_strategy.batch_size = size;
            updated_fields.push("submission_scan_batch_size");
        }
    }

    if let Some(enabled) = params.submission_adaptive_scan {
        if enabled != config.submission_scan_strategy.adaptive_enabled {
            config.submission_scan_strategy.adaptive_enabled = enabled;
            updated_fields.push("submission_adaptive_scan");
        }
    }

    if let Some(hours) = params.submission_adaptive_max_hours {
        if hours == 0 {
            return Err(anyhow!("自适应扫描最大间隔不能为 0").into());
        }
        let hours = hours.min(168); // 最高 7 天，避免误填导致长期不扫描
        if hours != config.submission_scan_strategy.adaptive_max_hours {
            config.submission_scan_strategy.adaptive_max_hours = hours;
            updated_fields.push("submission_adaptive_max_hours");
        }
    }

    // 处理多P视频目录结构配置
    if let Some(use_season_structure) = params.multi_page_use_season_structure {
        if use_season_structure != config.multi_page_use_season_structure {
            config.multi_page_use_season_structure = use_season_structure;
            updated_fields.push("multi_page_use_season_structure");
        }
    }

    // 处理合集目录结构配置
    if let Some(use_season_structure) = params.collection_use_season_structure {
        if use_season_structure != config.collection_use_season_structure {
            config.collection_use_season_structure = use_season_structure;
            updated_fields.push("collection_use_season_structure");
        }
    }

    // 处理番剧目录结构配置
    if let Some(use_season_structure) = params.bangumi_use_season_structure {
        if use_season_structure != config.bangumi_use_season_structure {
            config.bangumi_use_season_structure = use_season_structure;
            updated_fields.push("bangumi_use_season_structure");
        }
    }

    // UP主头像保存路径
    if let Some(upper_path) = params.upper_path {
        if !upper_path.trim().is_empty() {
            let new_path = std::path::PathBuf::from(upper_path);
            if new_path != config.upper_path {
                config.upper_path = new_path;
                updated_fields.push("upper_path");
            }
        }
    }

    if let Some(ffmpeg_path) = params.ffmpeg_path {
        let normalized = ffmpeg_path.trim().to_string();
        if normalized != config.ffmpeg_path {
            config.ffmpeg_path = normalized;
            updated_fields.push("ffmpeg_path");
        }
    }

    // 服务器绑定地址配置
    if let Some(bind_address) = params.bind_address {
        if !bind_address.trim().is_empty() {
            let normalized_address = if bind_address.contains(':') {
                // 已经包含端口，直接使用
                bind_address.clone()
            } else {
                // 只有端口号，添加默认IP
                if let Ok(port) = bind_address.parse::<u16>() {
                    if port == 0 {
                        return Err(anyhow!("端口号不能为0").into());
                    }
                    format!("0.0.0.0:{}", port)
                } else {
                    return Err(anyhow!("无效的端口号格式").into());
                }
            };

            // 验证地址格式
            if let Some(colon_pos) = normalized_address.rfind(':') {
                let (_ip, port_str) = normalized_address.split_at(colon_pos + 1);
                if let Ok(port) = port_str.parse::<u16>() {
                    if port == 0 {
                        return Err(anyhow!("端口号不能为0").into());
                    }
                } else {
                    return Err(anyhow!("无效的端口号格式").into());
                }
            } else {
                return Err(anyhow!("绑定地址格式无效，应为 'IP:端口' 或 '端口'").into());
            }

            if normalized_address != config.bind_address {
                config.bind_address = normalized_address;
                updated_fields.push("bind_address");
            }
        }
    }

    // 风控验证配置
    if let Some(enabled) = params.risk_control_enabled {
        if enabled != config.risk_control.enabled {
            config.risk_control.enabled = enabled;
            updated_fields.push("risk_control.enabled");
        }
    }

    if let Some(mode) = params.risk_control_mode {
        if !mode.trim().is_empty() && mode != config.risk_control.mode {
            // 验证模式的有效性
            match mode.as_str() {
                "manual" | "auto" | "skip" => {
                    config.risk_control.mode = mode;
                    updated_fields.push("risk_control.mode");
                }
                _ => {
                    return Err(anyhow!(
                        "无效的风控模式，只支持 'manual'（手动验证）、'auto'（自动验证）或 'skip'（跳过验证）"
                    )
                    .into());
                }
            }
        }
    }

    if let Some(timeout) = params.risk_control_timeout {
        if timeout > 0 && timeout != config.risk_control.timeout {
            config.risk_control.timeout = timeout;
            updated_fields.push("risk_control.timeout");
        }
    }

    // 自动验证配置处理
    if let Some(service) = params.risk_control_auto_solve_service {
        if !service.trim().is_empty() {
            // 验证服务的有效性
            match service.as_str() {
                "2captcha" | "anticaptcha" => {
                    // 如果auto_solve配置不存在，创建一个新的
                    if config.risk_control.auto_solve.is_none() {
                        config.risk_control.auto_solve = Some(crate::config::AutoSolveConfig {
                            service: service.clone(),
                            api_key: String::new(),
                            max_retries: 3,
                            solve_timeout: 120,
                        });
                        updated_fields.push("risk_control.auto_solve.service");
                    } else if config.risk_control.auto_solve.as_ref().unwrap().service != service {
                        config.risk_control.auto_solve.as_mut().unwrap().service = service;
                        updated_fields.push("risk_control.auto_solve.service");
                    }
                }
                _ => {
                    return Err(anyhow!("无效的验证码识别服务，只支持 '2captcha' 或 'anticaptcha'").into());
                }
            }
        }
    }

    if let Some(api_key) = params.risk_control_auto_solve_api_key {
        if !api_key.trim().is_empty() {
            // 如果auto_solve配置不存在，创建一个新的
            if config.risk_control.auto_solve.is_none() {
                config.risk_control.auto_solve = Some(crate::config::AutoSolveConfig {
                    service: "2captcha".to_string(),
                    api_key: api_key.clone(),
                    max_retries: 3,
                    solve_timeout: 120,
                });
                updated_fields.push("risk_control.auto_solve.api_key");
            } else if config.risk_control.auto_solve.as_ref().unwrap().api_key != api_key {
                config.risk_control.auto_solve.as_mut().unwrap().api_key = api_key;
                updated_fields.push("risk_control.auto_solve.api_key");
            }
        }
    }

    if let Some(max_retries) = params.risk_control_auto_solve_max_retries {
        if (1..=10).contains(&max_retries) {
            // 如果auto_solve配置不存在，创建一个新的
            if config.risk_control.auto_solve.is_none() {
                config.risk_control.auto_solve = Some(crate::config::AutoSolveConfig {
                    service: "2captcha".to_string(),
                    api_key: String::new(),
                    max_retries,
                    solve_timeout: 120,
                });
                updated_fields.push("risk_control.auto_solve.max_retries");
            } else if config.risk_control.auto_solve.as_ref().unwrap().max_retries != max_retries {
                config.risk_control.auto_solve.as_mut().unwrap().max_retries = max_retries;
                updated_fields.push("risk_control.auto_solve.max_retries");
            }
        }
    }

    if let Some(solve_timeout) = params.risk_control_auto_solve_timeout {
        if (30..=300).contains(&solve_timeout) {
            // 如果auto_solve配置不存在，创建一个新的
            if config.risk_control.auto_solve.is_none() {
                config.risk_control.auto_solve = Some(crate::config::AutoSolveConfig {
                    service: "2captcha".to_string(),
                    api_key: String::new(),
                    max_retries: 3,
                    solve_timeout,
                });
                updated_fields.push("risk_control.auto_solve.solve_timeout");
            } else if config.risk_control.auto_solve.as_ref().unwrap().solve_timeout != solve_timeout {
                config.risk_control.auto_solve.as_mut().unwrap().solve_timeout = solve_timeout;
                updated_fields.push("risk_control.auto_solve.solve_timeout");
            }
        }
    }

    // 处理AI重命名配置更新
    if let Some(enabled) = params.ai_rename_enabled {
        if config.ai_rename.enabled != enabled {
            config.ai_rename.enabled = enabled;
            updated_fields.push("ai_rename");
        }
    }
    if let Some(provider) = &params.ai_rename_provider {
        if config.ai_rename.provider != *provider {
            config.ai_rename.provider = provider.clone();
            updated_fields.push("ai_rename");
        }
    }
    if let Some(base_url) = &params.ai_rename_base_url {
        if config.ai_rename.base_url != *base_url {
            config.ai_rename.base_url = base_url.clone();
            updated_fields.push("ai_rename");
        }
    }
    if let Some(api_key) = &params.ai_rename_api_key {
        if config.ai_rename.api_key.as_ref() != Some(api_key) {
            config.ai_rename.api_key = if api_key.is_empty() {
                None
            } else {
                Some(api_key.clone())
            };
            updated_fields.push("ai_rename");
        }
    }
    if let Some(token) = &params.ai_rename_deepseek_web_token {
        if config.ai_rename.deepseek_web_token.as_ref() != Some(token) {
            config.ai_rename.deepseek_web_token = if token.is_empty() { None } else { Some(token.clone()) };
            updated_fields.push("ai_rename");
            // Token 更新后重置过期通知标志，以便下次过期时可以再次通知
            crate::utils::deepseek_web::reset_token_expired_flag();
        }
    }
    if let Some(model) = &params.ai_rename_model {
        if config.ai_rename.model != *model {
            config.ai_rename.model = model.clone();
            updated_fields.push("ai_rename");
        }
    }
    if let Some(timeout_seconds) = params.ai_rename_timeout_seconds {
        if config.ai_rename.timeout_seconds != timeout_seconds {
            config.ai_rename.timeout_seconds = timeout_seconds;
            updated_fields.push("ai_rename");
        }
    }
    if let Some(video_prompt_hint) = &params.ai_rename_video_prompt_hint {
        if config.ai_rename.video_prompt_hint != *video_prompt_hint {
            config.ai_rename.video_prompt_hint = video_prompt_hint.clone();
            updated_fields.push("ai_rename");
        }
    }
    if let Some(audio_prompt_hint) = &params.ai_rename_audio_prompt_hint {
        if config.ai_rename.audio_prompt_hint != *audio_prompt_hint {
            config.ai_rename.audio_prompt_hint = audio_prompt_hint.clone();
            updated_fields.push("ai_rename");
        }
    }
    if let Some(enable_multi_page) = params.ai_rename_enable_multi_page {
        if config.ai_rename.enable_multi_page != enable_multi_page {
            config.ai_rename.enable_multi_page = enable_multi_page;
            updated_fields.push("ai_rename");
        }
    }
    if let Some(enable_collection) = params.ai_rename_enable_collection {
        if config.ai_rename.enable_collection != enable_collection {
            config.ai_rename.enable_collection = enable_collection;
            updated_fields.push("ai_rename");
        }
    }
    if let Some(enable_bangumi) = params.ai_rename_enable_bangumi {
        if config.ai_rename.enable_bangumi != enable_bangumi {
            config.ai_rename.enable_bangumi = enable_bangumi;
            updated_fields.push("ai_rename");
        }
    }
    if let Some(rename_parent_dir) = params.ai_rename_rename_parent_dir {
        if config.ai_rename.rename_parent_dir != rename_parent_dir {
            config.ai_rename.rename_parent_dir = rename_parent_dir;
            updated_fields.push("ai_rename");
        }
    }

    if updated_fields.is_empty() {
        return Ok(crate::api::response::UpdateConfigResponse {
            success: false,
            message: "没有提供有效的配置更新".to_string(),
            updated_files: None,
            resetted_nfo_videos_count: None,
            resetted_nfo_pages_count: None,
        });
    }

    let updated_field_labels = format_config_update_fields_display(&updated_fields);
    let updated_fields_display = updated_field_labels.join("、");

    // 移除配置文件保存 - 配置现在完全基于数据库
    // config.save()?;

    // 根据 updated_fields 只更新被修改的配置项
    if !updated_fields.is_empty() {
        use crate::config::ConfigManager;
        let manager = ConfigManager::new(db.as_ref().clone());

        // 将 updated_fields 映射到实际的配置项更新
        for field in &updated_fields {
            let result = match *field {
                // 处理文件命名设置
                "video_name" => {
                    manager
                        .update_config_item("video_name", serde_json::to_value(&config.video_name)?)
                        .await
                }
                "page_name" => {
                    manager
                        .update_config_item("page_name", serde_json::to_value(&config.page_name)?)
                        .await
                }
                "multi_page_name" => {
                    manager
                        .update_config_item("multi_page_name", serde_json::to_value(&config.multi_page_name)?)
                        .await
                }
                "bangumi_name" => {
                    manager
                        .update_config_item("bangumi_name", serde_json::to_value(&config.bangumi_name)?)
                        .await
                }
                "folder_structure" => {
                    manager
                        .update_config_item("folder_structure", serde_json::to_value(&config.folder_structure)?)
                        .await
                }
                "bangumi_folder_name" => {
                    manager
                        .update_config_item(
                            "bangumi_folder_name",
                            serde_json::to_value(&config.bangumi_folder_name)?,
                        )
                        .await
                }
                "collection_folder_mode" => {
                    manager
                        .update_config_item(
                            "collection_folder_mode",
                            serde_json::to_value(&config.collection_folder_mode)?,
                        )
                        .await
                }
                "collection_unified_name" => {
                    manager
                        .update_config_item(
                            "collection_unified_name",
                            serde_json::to_value(&config.collection_unified_name)?,
                        )
                        .await
                }
                "time_format" => {
                    manager
                        .update_config_item("time_format", serde_json::to_value(&config.time_format)?)
                        .await
                }
                "interval" => {
                    manager
                        .update_config_item("interval", serde_json::to_value(config.interval)?)
                        .await
                }
                "nfo_time_type" => {
                    manager
                        .update_config_item("nfo_time_type", serde_json::to_value(&config.nfo_time_type)?)
                        .await
                }
                "upper_path" => {
                    manager
                        .update_config_item("upper_path", serde_json::to_value(&config.upper_path)?)
                        .await
                }
                "ffmpeg_path" => {
                    manager
                        .update_config_item("ffmpeg_path", serde_json::to_value(&config.ffmpeg_path)?)
                        .await
                }
                "bind_address" => {
                    manager
                        .update_config_item("bind_address", serde_json::to_value(&config.bind_address)?)
                        .await
                }
                "concurrent_limit" => {
                    manager
                        .update_config_item("concurrent_limit", serde_json::to_value(&config.concurrent_limit)?)
                        .await
                }
                "cdn_sorting" => {
                    manager
                        .update_config_item("cdn_sorting", serde_json::to_value(config.cdn_sorting)?)
                        .await
                }
                "scan_deleted_videos" => {
                    manager
                        .update_config_item("scan_deleted_videos", serde_json::to_value(config.scan_deleted_videos)?)
                        .await
                }
                "enable_aria2_health_check" => {
                    manager
                        .update_config_item(
                            "enable_aria2_health_check",
                            serde_json::to_value(config.enable_aria2_health_check)?,
                        )
                        .await
                }
                "enable_aria2_auto_restart" => {
                    manager
                        .update_config_item(
                            "enable_aria2_auto_restart",
                            serde_json::to_value(config.enable_aria2_auto_restart)?,
                        )
                        .await
                }
                "aria2_health_check_interval" => {
                    manager
                        .update_config_item(
                            "aria2_health_check_interval",
                            serde_json::to_value(config.aria2_health_check_interval)?,
                        )
                        .await
                }
                "submission_risk_control" => {
                    manager
                        .update_config_item(
                            "submission_risk_control",
                            serde_json::to_value(&config.submission_risk_control)?,
                        )
                        .await
                }
                // 对于复合字段，使用特殊处理
                "rate_limit"
                | "rate_duration"
                | "parallel_download_enabled"
                | "parallel_download_threads"
                | "parallel_download_use_aria2"
                | "concurrent_video"
                | "concurrent_page" => {
                    manager
                        .update_config_item("concurrent_limit", serde_json::to_value(&config.concurrent_limit)?)
                        .await
                }
                "large_submission_threshold"
                | "base_request_delay"
                | "large_submission_delay_multiplier"
                | "enable_progressive_delay"
                | "max_delay_multiplier"
                | "enable_incremental_fetch"
                | "incremental_fallback_to_full"
                | "enable_batch_processing"
                | "batch_size"
                | "batch_delay_seconds"
                | "enable_auto_backoff"
                | "auto_backoff_base_seconds"
                | "auto_backoff_max_multiplier"
                | "source_delay_seconds"
                | "submission_source_delay_seconds" => {
                    manager
                        .update_config_item(
                            "submission_risk_control",
                            serde_json::to_value(&config.submission_risk_control)?,
                        )
                        .await
                }
                "submission_scan_batch_size" | "submission_adaptive_scan" | "submission_adaptive_max_hours" => {
                    manager
                        .update_config_item(
                            "submission_scan_strategy",
                            serde_json::to_value(&config.submission_scan_strategy)?,
                        )
                        .await
                }
                // 处理视频质量相关字段
                "video_max_quality" | "video_min_quality" | "audio_max_quality" | "audio_min_quality" | "codecs"
                | "no_dolby_video" | "no_dolby_audio" | "no_hdr" | "no_hires" => {
                    manager
                        .update_config_item("filter_option", serde_json::to_value(&config.filter_option)?)
                        .await
                }
                // 处理弹幕相关字段
                "danmaku_duration"
                | "danmaku_font"
                | "danmaku_font_size"
                | "danmaku_width_ratio"
                | "danmaku_horizontal_gap"
                | "danmaku_lane_size"
                | "danmaku_float_percentage"
                | "danmaku_bottom_percentage"
                | "danmaku_opacity"
                | "danmaku_bold"
                | "danmaku_outline"
                | "danmaku_time_offset" => {
                    manager
                        .update_config_item("danmaku_option", serde_json::to_value(&config.danmaku_option)?)
                        .await
                }
                // NFO配置字段
                "nfo_config" => {
                    manager
                        .update_config_item("nfo_config", serde_json::to_value(&config.nfo_config)?)
                        .await
                }
                // 跳过番剧预告片
                "skip_bangumi_preview" => {
                    manager
                        .update_config_item(
                            "skip_bangumi_preview",
                            serde_json::to_value(config.skip_bangumi_preview)?,
                        )
                        .await
                }
                // Season结构配置字段
                "multi_page_use_season_structure" => {
                    manager
                        .update_config_item(
                            "multi_page_use_season_structure",
                            serde_json::to_value(config.multi_page_use_season_structure)?,
                        )
                        .await
                }
                "collection_use_season_structure" => {
                    manager
                        .update_config_item(
                            "collection_use_season_structure",
                            serde_json::to_value(config.collection_use_season_structure)?,
                        )
                        .await
                }
                "bangumi_use_season_structure" => {
                    manager
                        .update_config_item(
                            "bangumi_use_season_structure",
                            serde_json::to_value(config.bangumi_use_season_structure)?,
                        )
                        .await
                }
                // 通知配置字段
                "serverchan_key"
                | "enable_scan_notifications"
                | "notification_min_videos"
                | "notification_timeout"
                | "notification_retry_count" => {
                    manager
                        .update_config_item("notification", serde_json::to_value(&config.notification)?)
                        .await
                }
                // 风控配置字段
                "risk_control.enabled"
                | "risk_control.mode"
                | "risk_control.timeout"
                | "risk_control.auto_solve.service"
                | "risk_control.auto_solve.api_key"
                | "risk_control.auto_solve.max_retries"
                | "risk_control.auto_solve.solve_timeout" => {
                    manager
                        .update_config_item("risk_control", serde_json::to_value(&config.risk_control)?)
                        .await
                }
                // 启动时配置字段
                "enable_startup_data_fix" => {
                    manager
                        .update_config_item(
                            "enable_startup_data_fix",
                            serde_json::to_value(config.enable_startup_data_fix)?,
                        )
                        .await
                }
                "enable_cid_population" => {
                    manager
                        .update_config_item(
                            "enable_cid_population",
                            serde_json::to_value(config.enable_cid_population)?,
                        )
                        .await
                }
                // API Token
                "auth_token" => {
                    manager
                        .update_config_item("auth_token", serde_json::to_value(&config.auth_token)?)
                        .await
                }
                // actors字段初始化状态
                "actors_field_initialized" => {
                    manager
                        .update_config_item(
                            "actors_field_initialized",
                            serde_json::to_value(config.actors_field_initialized)?,
                        )
                        .await
                }
                // AI重命名配置字段
                "ai_rename"
                | "ai_rename.enabled"
                | "ai_rename.provider"
                | "ai_rename.base_url"
                | "ai_rename.api_key"
                | "ai_rename.model"
                | "ai_rename.timeout_seconds"
                | "ai_rename.video_prompt_hint"
                | "ai_rename.audio_prompt_hint" => {
                    manager
                        .update_config_item("ai_rename", serde_json::to_value(&config.ai_rename)?)
                        .await
                }
                _ => {
                    warn!("未知的配置字段: {}", field);
                    Ok(())
                }
            };

            if let Err(e) = result {
                warn!("更新配置项 {} 失败: {}", field, e);
            }
        }

        info!(
            "已更新 {} 个配置项: {}",
            updated_field_labels.len(),
            updated_fields_display
        );
    } else {
        info!("没有配置项需要更新");
    }

    // 重新加载全局配置包（从数据库）
    if let Err(e) = crate::config::reload_config_bundle().await {
        warn!("重新加载配置包失败: {}", e);
        // 回退到传统的重新加载方式
        crate::config::reload_config();
    }

    // 如果更新了命名相关的配置，重命名已下载的文件
    let mut updated_files = 0u32;
    let naming_fields = [
        "video_name",
        "page_name",
        "multi_page_name",
        "bangumi_name",
        "folder_structure",
        "bangumi_folder_name",
    ];
    let should_rename = updated_fields.iter().any(|field| naming_fields.contains(field));

    if should_rename {
        // 暂停定时扫描任务，避免与重命名操作产生数据库锁定冲突
        crate::task::pause_scanning().await;
        info!("重命名操作开始，已暂停定时扫描任务");

        // 根据更新的字段类型来决定重命名哪些文件
        let rename_single_page = updated_fields.contains(&"page_name") || updated_fields.contains(&"video_name");
        let rename_multi_page = updated_fields.contains(&"multi_page_name") || updated_fields.contains(&"video_name");
        let rename_bangumi = updated_fields.contains(&"bangumi_name") || updated_fields.contains(&"video_name");
        let rename_folder_structure = updated_fields.contains(&"folder_structure");

        // 重新获取最新的配置，确保使用重新加载后的配置
        let latest_config = crate::config::with_config(|bundle| bundle.config.clone());

        // 执行文件重命名并等待完成
        match rename_existing_files(
            db.clone(),
            &latest_config,
            rename_single_page,
            rename_multi_page,
            rename_bangumi,
            rename_folder_structure,
        )
        .await
        {
            Ok(count) => {
                updated_files = count;
                info!("重命名操作完成，共处理了 {} 个文件/文件夹", count);
            }
            Err(e) => {
                error!("重命名已下载文件时出错: {}", e);
                // 即使重命名失败，配置更新仍然成功
            }
        }

        // 恢复定时扫描任务
        crate::task::resume_scanning();
        info!("重命名操作结束，已恢复定时扫描任务");
    }

    // 检查是否需要重置NFO任务状态
    let should_reset_nfo = updated_fields.contains(&"nfo_time_type");
    let mut resetted_nfo_videos_count = 0;
    let mut resetted_nfo_pages_count = 0;

    if should_reset_nfo {
        // 重置NFO任务状态
        match reset_nfo_tasks_for_config_change(db.clone()).await {
            Ok((videos_count, pages_count)) => {
                resetted_nfo_videos_count = videos_count;
                resetted_nfo_pages_count = pages_count;
                info!(
                    "NFO任务状态重置成功，重置了 {} 个视频和 {} 个页面",
                    videos_count, pages_count
                );

                // 如果有任务被重置，触发立即扫描来处理重置的NFO任务
                if videos_count > 0 || pages_count > 0 {
                    info!("准备触发立即扫描来处理重置的NFO任务");
                    crate::task::resume_scanning();
                    info!("NFO任务重置完成，已成功触发立即扫描");
                } else {
                    info!("没有NFO任务需要重置，跳过扫描触发");
                }
            }
            Err(e) => {
                error!("重置NFO任务状态时出错: {}", e);
                // 即使重置失败，配置更新仍然成功
            }
        }
    }

    // 内存优化已经通过mmap实现，不再需要动态配置

    Ok(crate::api::response::UpdateConfigResponse {
        success: true,
        message: if should_rename && should_reset_nfo {
            format!(
                "配置更新成功，已更新字段: {}，重命名了 {} 个文件/文件夹，重置了 {} 个视频和 {} 个页面的NFO任务并已触发立即扫描",
                updated_fields_display,
                updated_files,
                resetted_nfo_videos_count,
                resetted_nfo_pages_count
            )
        } else if should_rename {
            format!(
                "配置更新成功，已更新字段: {}，重命名了 {} 个文件/文件夹",
                updated_fields_display, updated_files
            )
        } else if should_reset_nfo {
            if resetted_nfo_videos_count > 0 || resetted_nfo_pages_count > 0 {
                format!(
                    "配置更新成功，已更新字段: {}，重置了 {} 个视频和 {} 个页面的NFO任务并已触发立即扫描",
                    updated_fields_display, resetted_nfo_videos_count, resetted_nfo_pages_count
                )
            } else {
                format!(
                    "配置更新成功，已更新字段: {}，没有找到需要重置的NFO任务",
                    updated_fields_display
                )
            }
        } else {
            format!("配置更新成功，已更新字段: {}", updated_fields_display)
        },
        updated_files: if should_rename { Some(updated_files) } else { None },
        resetted_nfo_videos_count: if should_reset_nfo {
            Some(resetted_nfo_videos_count)
        } else {
            None
        },
        resetted_nfo_pages_count: if should_reset_nfo {
            Some(resetted_nfo_pages_count)
        } else {
            None
        },
    })
}

/// 查找分页文件的原始命名模式
fn find_page_file_pattern(video_path: &std::path::Path, page: &bili_sync_entity::page::Model) -> Result<String> {
    // 首先尝试在主目录查找
    if let Some(pattern) = find_page_file_in_dir(video_path, page) {
        return Ok(pattern);
    }

    // 如果主目录没找到，尝试在Season子目录中查找
    // 检查所有Season格式的子目录
    if video_path.exists() {
        if let Ok(entries) = std::fs::read_dir(video_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                    if dir_name.starts_with("Season") {
                        if let Some(pattern) = find_page_file_in_dir(&path, page) {
                            return Ok(pattern);
                        }
                    }
                }
            }
        }
    }

    Ok(String::new())
}

/// 在指定目录中查找分页文件
fn find_page_file_in_dir(dir_path: &std::path::Path, page: &bili_sync_entity::page::Model) -> Option<String> {
    if !dir_path.exists() {
        return None;
    }

    if let Ok(entries) = std::fs::read_dir(dir_path) {
        for entry in entries.flatten() {
            let file_path = entry.path();
            let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();

            // 尝试通过文件名中的分页编号来匹配主文件（MP4）
            if file_name.ends_with(".mp4")
                && (file_name.contains(&format!("{:02}", page.pid))
                    || file_name.contains(&format!("{:03}", page.pid))
                    || file_name.contains(&page.name))
            {
                // 找到MP4文件，提取文件名（不包括扩展名）
                if let Some(file_stem) = file_path.file_stem() {
                    return Some(file_stem.to_string_lossy().to_string());
                }
            }
        }
    }

    None
}

/// 重命名已下载的文件以匹配新的命名规则
#[allow(unused_variables)] // rename_folder_structure 参数表示是否更新了 folder_structure 配置，虽然当前未使用但保留以备将来扩展
async fn rename_existing_files(
    db: Arc<DatabaseConnection>,
    config: &crate::config::Config,
    rename_single_page: bool,
    rename_multi_page: bool,
    rename_bangumi: bool,
    rename_folder_structure: bool,
) -> Result<u32> {
    use handlebars::{handlebars_helper, Handlebars};
    use sea_orm::*;
    use std::path::Path;

    info!("开始重命名已下载的文件以匹配新的配置...");

    let mut updated_count = 0u32;

    // 创建模板引擎
    let mut handlebars = Handlebars::new();

    // **关键修复：注册所有必要的helper函数，确保与下载时使用相同的模板引擎功能**
    handlebars_helper!(truncate: |s: String, len: usize| {
        if s.chars().count() > len {
            s.chars().take(len).collect::<String>()
        } else {
            s.to_string()
        }
    });
    handlebars.register_helper("truncate", Box::new(truncate));

    // 使用register_template_string而不是path_safe_register来避免生命周期问题
    // 同时处理正斜杠和反斜杠，确保跨平台兼容性
    // **修复：使用更唯一的分隔符标记，避免与文件名中的下划线冲突**
    let video_template = config.video_name.replace(['/', '\\'], "___PATH_SEP___");
    let page_template = config.page_name.replace(['/', '\\'], "___PATH_SEP___");
    let multi_page_template = config.multi_page_name.replace(['/', '\\'], "___PATH_SEP___");
    let bangumi_template = config.bangumi_name.replace(['/', '\\'], "___PATH_SEP___");

    info!("🔧 原始视频模板: '{}'", config.video_name);
    info!("🔧 处理后视频模板: '{}'", video_template);
    info!("🔧 原始番剧模板: '{}'", config.bangumi_name);
    info!("🔧 处理后番剧模板: '{}'", bangumi_template);
    info!("🔧 从配置中读取的bangumi_name: '{}'", config.bangumi_name);

    handlebars.register_template_string("video", video_template)?;
    handlebars.register_template_string("page", page_template)?;
    handlebars.register_template_string("multi_page", multi_page_template)?;
    handlebars.register_template_string("bangumi", bangumi_template)?;

    // 分别处理不同类型的视频
    let mut all_videos = Vec::new();

    // 1. 处理非番剧类型的视频（原有逻辑）
    if rename_single_page || rename_multi_page {
        let regular_videos = bili_sync_entity::video::Entity::find()
            .filter(bili_sync_entity::video::Column::DownloadStatus.gt(0))
            .filter(
                // 排除番剧类型（source_type=1），包含其他所有类型
                bili_sync_entity::video::Column::SourceType
                    .is_null()
                    .or(bili_sync_entity::video::Column::SourceType.ne(1)),
            )
            .all(db.as_ref())
            .await?;
        all_videos.extend(regular_videos);
    }

    // 2. 处理番剧类型的视频
    if rename_bangumi {
        let bangumi_videos = bili_sync_entity::video::Entity::find()
            .filter(bili_sync_entity::video::Column::DownloadStatus.gt(0))
            .filter(bili_sync_entity::video::Column::SourceType.eq(1)) // 番剧类型
            .all(db.as_ref())
            .await?;
        all_videos.extend(bangumi_videos);
    }

    info!("找到 {} 个需要检查的视频", all_videos.len());

    for video in all_videos {
        // 检查视频类型，决定是否需要重命名
        let is_single_page = video.single_page.unwrap_or(true);
        let is_bangumi = video.source_type == Some(1);
        let is_collection = video.collection_id.is_some();

        // 根据视频类型和配置更新情况决定是否跳过
        let should_process_video = if is_bangumi {
            rename_bangumi // 番剧视频只在bangumi_name或video_name更新时处理
        } else if is_collection {
            rename_multi_page // 合集视频使用多P视频的重命名逻辑，但需要特殊处理
        } else if is_single_page {
            rename_single_page // 单P视频只在page_name或video_name更新时处理
        } else {
            rename_multi_page // 多P视频只在multi_page_name或video_name更新时处理
        };

        if !should_process_video {
            let video_type = if is_bangumi {
                "番剧"
            } else if is_collection {
                "合集"
            } else if is_single_page {
                "单P"
            } else {
                "多P"
            };
            debug!("跳过视频重命名: {} (类型: {})", video.name, video_type);
            continue;
        }

        // 构建模板数据
        let mut template_data = std::collections::HashMap::new();

        // 对于合集视频，需要获取合集名称
        let collection_name = if is_collection {
            if let Some(collection_id) = video.collection_id {
                match bili_sync_entity::collection::Entity::find_by_id(collection_id)
                    .one(db.as_ref())
                    .await
                {
                    Ok(Some(collection)) => Some(collection.name),
                    Ok(None) => {
                        warn!("合集ID {} 不存在", collection_id);
                        None
                    }
                    Err(e) => {
                        error!("查询合集信息失败: {}", e);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        // 设置title: 合集使用合集名称，其他使用视频名称
        let display_title = if let Some(ref coll_name) = collection_name {
            coll_name.clone()
        } else {
            video.name.clone()
        };

        template_data.insert("title".to_string(), serde_json::Value::String(display_title.clone()));
        template_data.insert("show_title".to_string(), serde_json::Value::String(display_title));
        template_data.insert("bvid".to_string(), serde_json::Value::String(video.bvid.clone()));
        template_data.insert(
            "upper_name".to_string(),
            serde_json::Value::String(video.upper_name.clone()),
        );
        template_data.insert(
            "upper_mid".to_string(),
            serde_json::Value::String(video.upper_id.to_string()),
        );

        // 为番剧视频添加特殊变量
        if is_bangumi {
            // 从视频名称提取 series_title
            let series_title = extract_bangumi_series_title(&video.name);
            let season_title = extract_bangumi_season_title(&video.name);

            template_data.insert("series_title".to_string(), serde_json::Value::String(series_title));
            template_data.insert("season_title".to_string(), serde_json::Value::String(season_title));

            // 添加其他番剧相关变量
            template_data.insert(
                "season_number".to_string(),
                serde_json::Value::Number(serde_json::Number::from(video.season_number.unwrap_or(1))),
            );
            template_data.insert(
                "episode_number".to_string(),
                serde_json::Value::Number(serde_json::Number::from(video.episode_number.unwrap_or(1))),
            );
            template_data.insert(
                "season".to_string(),
                serde_json::Value::String(video.season_number.unwrap_or(1).to_string()),
            );
            template_data.insert(
                "season_pad".to_string(),
                serde_json::Value::String(format!("{:02}", video.season_number.unwrap_or(1))),
            );
            template_data.insert(
                "episode".to_string(),
                serde_json::Value::String(video.episode_number.unwrap_or(1).to_string()),
            );
            template_data.insert(
                "episode_pad".to_string(),
                serde_json::Value::String(format!("{:02}", video.episode_number.unwrap_or(1))),
            );

            // 添加其他信息
            if let Some(ref season_id) = video.season_id {
                template_data.insert("season_id".to_string(), serde_json::Value::String(season_id.clone()));
            }
            if let Some(ref ep_id) = video.ep_id {
                template_data.insert("ep_id".to_string(), serde_json::Value::String(ep_id.clone()));
            }
            if let Some(ref share_copy) = video.share_copy {
                template_data.insert("share_copy".to_string(), serde_json::Value::String(share_copy.clone()));
            }
            if let Some(ref actors) = video.actors {
                template_data.insert("actors".to_string(), serde_json::Value::String(actors.clone()));
            }

            // 添加年份
            template_data.insert(
                "year".to_string(),
                serde_json::Value::Number(serde_json::Number::from(video.pubtime.year())),
            );
            template_data.insert(
                "studio".to_string(),
                serde_json::Value::String(video.upper_name.clone()),
            );
        }

        // 为合集添加额外的模板变量
        if let Some(ref coll_name) = collection_name {
            template_data.insert(
                "collection_name".to_string(),
                serde_json::Value::String(coll_name.clone()),
            );
            template_data.insert("video_name".to_string(), serde_json::Value::String(video.name.clone()));
        }

        // 格式化时间
        let formatted_pubtime = video.pubtime.format(&config.time_format).to_string();
        template_data.insert(
            "pubtime".to_string(),
            serde_json::Value::String(formatted_pubtime.clone()),
        );

        let formatted_favtime = video.favtime.format(&config.time_format).to_string();
        template_data.insert("fav_time".to_string(), serde_json::Value::String(formatted_favtime));

        let formatted_ctime = video.ctime.format(&config.time_format).to_string();
        template_data.insert("ctime".to_string(), serde_json::Value::String(formatted_ctime));

        // 确定最终的视频文件夹路径
        let final_video_path = if is_bangumi {
            // 番剧不重命名视频文件夹，直接使用现有路径
            let video_path = Path::new(&video.path);
            if video_path.exists() {
                video_path.to_path_buf()
            } else {
                // 如果路径不存在，尝试智能查找
                if let Some(parent_dir) = video_path.parent() {
                    if let Ok(entries) = std::fs::read_dir(parent_dir) {
                        let mut found_path = None;
                        for entry in entries.flatten() {
                            let entry_path = entry.path();
                            if entry_path.is_dir() {
                                let dir_name = entry_path.file_name().unwrap_or_default().to_string_lossy();
                                // 检查是否包含视频的bvid或标题
                                if dir_name.contains(&video.bvid) || dir_name.contains(&video.name) {
                                    found_path = Some(entry_path);
                                    break;
                                }
                            }
                        }
                        found_path.unwrap_or_else(|| video_path.to_path_buf())
                    } else {
                        video_path.to_path_buf()
                    }
                } else {
                    video_path.to_path_buf()
                }
            }
        } else {
            // 非番剧视频的重命名逻辑（改进的智能重组逻辑）
            // 渲染新的视频文件夹名称（使用video_name模板）
            let template_value = serde_json::Value::Object(template_data.clone().into_iter().collect());
            let rendered_name = handlebars
                .render("video", &template_value)
                .unwrap_or_else(|_| video.name.clone());

            info!("🔧 模板渲染结果: '{}'", rendered_name);
            // **最终修复：使用分段处理保持目录结构同时确保文件名安全**
            let base_video_name = process_path_with_filenamify(&rendered_name);
            info!("🔧 路径处理完成: '{}'", base_video_name);

            // 使用视频记录中的路径信息
            let video_path = Path::new(&video.path);

            // **修复重复目录层级问题：重命名时只使用模板的最后一部分**
            // 如果模板生成的路径包含目录结构（如 "庄心妍/庄心妍的采访"）
            // 在重命名时应该只使用最后的文件夹名部分，避免创建重复层级
            let final_folder_name = if base_video_name.contains('/') {
                // 模板包含路径分隔符，只取最后一部分作为文件夹名
                let parts: Vec<&str> = base_video_name.split('/').collect();
                let last_part = parts
                    .last()
                    .map(|s| (*s).to_owned())
                    .unwrap_or_else(|| base_video_name.clone());
                info!(
                    "🔧 模板包含路径分隔符，重命名时只使用最后部分: '{}' -> '{}'",
                    base_video_name, last_part
                );
                last_part
            } else {
                // 模板不包含路径分隔符，直接使用
                base_video_name.clone()
            };

            // 使用当前视频的父目录作为基础路径
            let base_parent_dir = video_path.parent().unwrap_or(Path::new("."));

            if base_parent_dir.exists() {
                // **智能判断：根据模板内容决定是否需要去重**
                // 如果模板包含会产生相同名称的变量（如upper_name），则不使用智能去重
                // 如果模板包含会产生不同名称的变量（如title），则使用智能去重避免冲突
                let video_template = config.video_name.as_ref();
                let basic_needs_deduplication = video_template.contains("title")
                    || video_template.contains("name") && !video_template.contains("upper_name");

                // **修复：为合集和多P视频的Season结构添加例外处理**
                // 对于启用Season结构的合集和多P视频，相同路径是期望行为，不应该被当作冲突
                let should_skip_deduplication =
                    // 合集视频且启用合集Season结构
                    (is_collection && config.collection_use_season_structure) ||
                    // 多P视频且启用多P Season结构
                    (!is_single_page && config.multi_page_use_season_structure);

                let needs_deduplication = basic_needs_deduplication && !should_skip_deduplication;

                if should_skip_deduplication {
                    info!(
                        "🔧 跳过冲突检测: 视频 {} (合集: {}, 多P Season: {}, 合集 Season: {})",
                        video.bvid,
                        is_collection,
                        !is_single_page && config.multi_page_use_season_structure,
                        is_collection && config.collection_use_season_structure
                    );
                }

                let expected_new_path = if needs_deduplication {
                    // 使用智能去重生成唯一文件夹名
                    let unique_folder_name = generate_unique_folder_name(
                        base_parent_dir,
                        &final_folder_name,
                        &video.bvid,
                        &formatted_pubtime,
                    );
                    base_parent_dir.join(&unique_folder_name)
                } else {
                    // 不使用去重，允许多个视频共享同一文件夹
                    base_parent_dir.join(&final_folder_name)
                };

                // **修复分离逻辑：从合并文件夹中提取单个视频的文件**
                // 智能查找包含此视频文件的文件夹
                let source_folder_with_files = if video_path.exists() {
                    Some(video_path.to_path_buf())
                } else {
                    // 在父目录中查找包含此视频文件的文件夹
                    // 先尝试在原父目录查找，如果找不到再尝试基础父目录
                    let search_dirs = if let Some(original_parent) = video_path.parent() {
                        if original_parent != base_parent_dir {
                            vec![original_parent, base_parent_dir]
                        } else {
                            vec![base_parent_dir]
                        }
                    } else {
                        vec![base_parent_dir]
                    };

                    let mut found_path = None;
                    for search_dir in search_dirs {
                        if let Ok(entries) = std::fs::read_dir(search_dir) {
                            for entry in entries.flatten() {
                                let entry_path = entry.path();
                                if entry_path.is_dir() {
                                    // 检查文件夹内是否包含属于此视频的文件
                                    if let Ok(files) = std::fs::read_dir(&entry_path) {
                                        for file_entry in files.flatten() {
                                            let file_name_os = file_entry.file_name();
                                            let file_name = file_name_os.to_string_lossy();
                                            // 通过bvid匹配文件
                                            if file_name.contains(&video.bvid) {
                                                found_path = Some(entry_path.clone());
                                                break;
                                            }
                                        }
                                        if found_path.is_some() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        if found_path.is_some() {
                            break;
                        }
                    }
                    found_path
                };

                // 处理文件提取和移动的情况
                if let Some(source_path) = source_folder_with_files {
                    if source_path != expected_new_path {
                        // 需要从源文件夹中提取属于此视频的文件
                        match extract_video_files_by_database(db.as_ref(), video.id, &expected_new_path).await {
                            Ok(_) => {
                                info!(
                                    "从共享文件夹提取视频文件成功: {:?} -> {:?} (bvid: {})",
                                    source_path, expected_new_path, video.bvid
                                );
                                updated_count += 1;
                                expected_new_path.clone()
                            }
                            Err(e) => {
                                warn!(
                                    "从共享文件夹提取视频文件失败: {:?} -> {:?}, 错误: {}",
                                    source_path, expected_new_path, e
                                );
                                source_path.clone()
                            }
                        }
                    } else {
                        // 文件夹已经是正确的名称和位置
                        source_path.clone()
                    }
                } else {
                    // 文件夹不存在，使用新路径
                    expected_new_path.clone()
                }
            } else {
                video_path.to_path_buf()
            }
        };

        // **关键修复：始终更新数据库中的路径记录**
        // 不管文件夹是否重命名，都要确保数据库路径与实际文件系统路径一致
        let final_path_str = final_video_path.to_string_lossy().to_string();
        if video.path != final_path_str {
            let mut video_update: bili_sync_entity::video::ActiveModel = video.clone().into();
            video_update.path = Set(final_path_str.clone());
            if let Err(e) = video_update.update(db.as_ref()).await {
                warn!("更新数据库中的视频路径失败: {}", e);
            } else {
                debug!("更新数据库视频路径: {} -> {}", video.path, final_path_str);
            }
        }

        // **新增：处理视频级别的文件重命名（poster、fanart、nfo）**
        // 只对非番剧的多P视频进行视频级别文件重命名
        if !is_single_page && !is_bangumi {
            // 多P视频需要重命名视频级别的文件
            let old_video_name = Path::new(&video.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| video.name.clone());

            let new_video_name = final_video_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| video.name.clone());

            if old_video_name != new_video_name {
                // 重命名视频级别的文件
                let video_level_files = [
                    (
                        format!("{}-thumb.jpg", old_video_name),
                        format!("{}-thumb.jpg", new_video_name),
                    ),
                    (
                        format!("{}-fanart.jpg", old_video_name),
                        format!("{}-fanart.jpg", new_video_name),
                    ),
                    (format!("{}.nfo", old_video_name), format!("{}.nfo", new_video_name)),
                    // 兼容旧的硬编码文件名
                    ("poster.jpg".to_string(), format!("{}-thumb.jpg", new_video_name)),
                    ("fanart.jpg".to_string(), format!("{}-fanart.jpg", new_video_name)),
                    ("tvshow.nfo".to_string(), format!("{}.nfo", new_video_name)),
                ];

                for (old_file_name, new_file_name) in video_level_files {
                    let old_file_path = final_video_path.join(&old_file_name);
                    let new_file_path = final_video_path.join(&new_file_name);

                    if old_file_path.exists() && old_file_path != new_file_path {
                        // **关键修复：检查目标文件是否已存在，避免覆盖**
                        let final_new_file_path = if new_file_path.exists() {
                            // 目标文件已存在，生成唯一文件名避免覆盖
                            let file_stem = new_file_path.file_stem().unwrap_or_default().to_string_lossy();
                            let file_extension = new_file_path.extension().unwrap_or_default().to_string_lossy();
                            let parent_dir = new_file_path.parent().unwrap_or(&final_video_path);

                            // 尝试添加BV号后缀避免冲突
                            let bvid_suffix = &video.bvid;
                            let unique_name = if file_extension.is_empty() {
                                format!("{}-{}", file_stem, bvid_suffix)
                            } else {
                                format!("{}-{}.{}", file_stem, bvid_suffix, file_extension)
                            };
                            let unique_path = parent_dir.join(unique_name);

                            // 如果BV号后缀仍然冲突，使用时间戳
                            if unique_path.exists() {
                                let timestamp = chrono::Local::now().format("%H%M%S").to_string();
                                let final_name = if file_extension.is_empty() {
                                    format!("{}-{}-{}", file_stem, bvid_suffix, timestamp)
                                } else {
                                    format!("{}-{}-{}.{}", file_stem, bvid_suffix, timestamp, file_extension)
                                };
                                parent_dir.join(final_name)
                            } else {
                                unique_path
                            }
                        } else {
                            new_file_path.clone()
                        };

                        match std::fs::rename(&old_file_path, &final_new_file_path) {
                            Ok(_) => {
                                if final_new_file_path != new_file_path {
                                    warn!(
                                        "检测到视频级别文件名冲突，已重命名避免覆盖: {:?} -> {:?}",
                                        old_file_path, final_new_file_path
                                    );
                                } else {
                                    debug!(
                                        "重命名视频级别文件成功: {:?} -> {:?}",
                                        old_file_path, final_new_file_path
                                    );
                                }
                                updated_count += 1;
                            }
                            Err(e) => {
                                warn!(
                                    "重命名视频级别文件失败: {:?} -> {:?}, 错误: {}",
                                    old_file_path, final_new_file_path, e
                                );
                            }
                        }
                    }
                }
            }
        }

        // 处理分页视频的重命名
        let pages = bili_sync_entity::page::Entity::find()
            .filter(bili_sync_entity::page::Column::VideoId.eq(video.id))
            .filter(bili_sync_entity::page::Column::DownloadStatus.gt(0))
            .all(db.as_ref())
            .await?;

        for page in pages {
            // 为分页添加额外的模板数据
            let mut page_template_data = template_data.clone();
            page_template_data.insert("ptitle".to_string(), serde_json::Value::String(page.name.clone()));
            page_template_data.insert("pid".to_string(), serde_json::Value::String(page.pid.to_string()));
            page_template_data.insert(
                "pid_pad".to_string(),
                serde_json::Value::String(format!("{:02}", page.pid)),
            );

            // 为多P视频和番剧添加season相关变量
            if !is_single_page || is_bangumi {
                if is_bangumi {
                    // 番剧需要添加 series_title 等变量
                    let series_title = extract_bangumi_series_title(&video.name);
                    let season_title = extract_bangumi_season_title(&video.name);

                    page_template_data.insert("series_title".to_string(), serde_json::Value::String(series_title));
                    page_template_data.insert("season_title".to_string(), serde_json::Value::String(season_title));

                    // 添加其他番剧特有变量
                    if let Some(ref share_copy) = video.share_copy {
                        page_template_data
                            .insert("share_copy".to_string(), serde_json::Value::String(share_copy.clone()));
                    }
                    if let Some(ref actors) = video.actors {
                        page_template_data.insert("actors".to_string(), serde_json::Value::String(actors.clone()));
                    }
                    page_template_data.insert(
                        "year".to_string(),
                        serde_json::Value::Number(serde_json::Number::from(video.pubtime.year())),
                    );
                    page_template_data.insert(
                        "studio".to_string(),
                        serde_json::Value::String(video.upper_name.clone()),
                    );
                }

                let season_number = if is_bangumi {
                    video.season_number.unwrap_or(1)
                } else {
                    1
                };
                let episode_number = if is_bangumi {
                    video.episode_number.unwrap_or(page.pid)
                } else {
                    page.pid
                };

                page_template_data.insert(
                    "season".to_string(),
                    serde_json::Value::String(season_number.to_string()),
                );
                page_template_data.insert(
                    "season_pad".to_string(),
                    serde_json::Value::String(format!("{:02}", season_number)),
                );
                page_template_data.insert("pid".to_string(), serde_json::Value::String(episode_number.to_string()));
                page_template_data.insert(
                    "pid_pad".to_string(),
                    serde_json::Value::String(format!("{:02}", episode_number)),
                );
                page_template_data.insert(
                    "episode".to_string(),
                    serde_json::Value::String(episode_number.to_string()),
                );
                page_template_data.insert(
                    "episode_pad".to_string(),
                    serde_json::Value::String(format!("{:02}", episode_number)),
                );
            }

            page_template_data.insert(
                "duration".to_string(),
                serde_json::Value::String(page.duration.to_string()),
            );

            if let Some(width) = page.width {
                page_template_data.insert("width".to_string(), serde_json::Value::String(width.to_string()));
            }

            if let Some(height) = page.height {
                page_template_data.insert("height".to_string(), serde_json::Value::String(height.to_string()));
            }

            // 根据视频类型选择不同的模板
            let page_template_value = serde_json::Value::Object(page_template_data.into_iter().collect());
            let rendered_page_name = if is_bangumi {
                // 番剧使用bangumi_name模板
                match handlebars.render("bangumi", &page_template_value) {
                    Ok(rendered) => rendered,
                    Err(e) => {
                        // 如果渲染失败，使用默认番剧格式
                        warn!("番剧模板渲染失败: {}", e);
                        let season_number = video.season_number.unwrap_or(1);
                        let episode_number = video.episode_number.unwrap_or(page.pid);
                        format!("S{:02}E{:02}-{:02}", season_number, episode_number, episode_number)
                    }
                }
            } else if is_single_page {
                // 单P视频使用page_name模板
                match handlebars.render("page", &page_template_value) {
                    Ok(rendered) => {
                        debug!("单P视频模板渲染成功: '{}' -> '{}'", config.page_name, rendered);
                        rendered
                    }
                    Err(e) => {
                        warn!(
                            "单P视频模板渲染失败: '{}', 错误: {}, 使用默认名称: '{}'",
                            config.page_name, e, page.name
                        );
                        page.name.clone()
                    }
                }
            } else {
                // 多P视频使用multi_page_name模板
                match handlebars.render("multi_page", &page_template_value) {
                    Ok(rendered) => rendered,
                    Err(e) => {
                        // 如果渲染失败，使用默认格式
                        warn!("多P模板渲染失败: {}", e);
                        format!("S01E{:02}-{:02}", page.pid, page.pid)
                    }
                }
            };

            // **最终修复：使用分段处理保持目录结构同时确保文件名安全**
            let new_page_name = process_path_with_filenamify(&rendered_page_name);

            // **关键修复：重命名分页的所有相关文件**
            // 从数据库存储的路径或智能查找中获取原始文件名模式（去掉扩展名）
            let old_page_name = if let Some(stored_path) = &page.path {
                let stored_file_path = Path::new(stored_path);
                if let Some(file_stem) = stored_file_path.file_stem() {
                    file_stem.to_string_lossy().to_string()
                } else {
                    // 如果无法从存储路径提取，尝试智能查找
                    find_page_file_pattern(&final_video_path, &page)?
                }
            } else {
                // 数据库中没有存储路径，尝试智能查找
                find_page_file_pattern(&final_video_path, &page)?
            };

            // 如果找到了原始文件名模式，重命名所有相关文件
            if !old_page_name.is_empty() && old_page_name != new_page_name {
                debug!(
                    "准备重命名分页 {} 的文件：{} -> {}",
                    page.pid, old_page_name, new_page_name
                );

                // 根据page的path确定实际文件所在目录
                let actual_file_dir = if let Some(ref page_path) = page.path {
                    // 从page.path中提取目录路径
                    let page_file_path = Path::new(page_path);
                    if let Some(parent) = page_file_path.parent() {
                        PathBuf::from(parent)
                    } else {
                        final_video_path.clone()
                    }
                } else {
                    // 如果page.path为空，尝试在Season子目录中查找
                    // 对于使用Season结构的视频，文件可能在Season子目录中
                    let season_dir = if is_bangumi && config.bangumi_use_season_structure {
                        // 番剧使用Season结构
                        let season_number = video.season_number.unwrap_or(1);
                        final_video_path.join(format!("Season {:02}", season_number))
                    } else if !is_single_page && config.multi_page_use_season_structure {
                        // 多P视频使用Season结构
                        final_video_path.join("Season 01")
                    } else if is_collection && config.collection_use_season_structure {
                        // 合集使用Season结构
                        final_video_path.join("Season 01")
                    } else {
                        final_video_path.clone()
                    };

                    // 检查Season目录是否存在
                    if season_dir.exists() {
                        season_dir
                    } else {
                        final_video_path.clone()
                    }
                };

                if actual_file_dir.exists() {
                    debug!("检查目录: {:?}", actual_file_dir);
                    if let Ok(entries) = std::fs::read_dir(&actual_file_dir) {
                        let mut found_any_file = false;
                        for entry in entries.flatten() {
                            let file_path = entry.path();
                            let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();

                            // 记录所有文件以便调试
                            if !found_any_file {
                                debug!("目录中的文件: {}", file_name);
                                found_any_file = true;
                            }

                            // 检查文件是否属于当前分页（使用原始文件名模式匹配）
                            // 匹配规则：文件名以原始模式开头，后面可以跟扩展名或其他后缀
                            if file_name.starts_with(&old_page_name) {
                                debug!("找到匹配文件: {} (匹配模式: {})", file_name, old_page_name);
                                // 提取原始文件名后面的部分（扩展名和其他后缀）
                                let suffix = file_name.strip_prefix(&old_page_name).unwrap_or("");

                                // 构建新的文件名：新模式 + 原有的后缀
                                let new_file_name = format!("{}{}", new_page_name, suffix);
                                let new_file_path = actual_file_dir.join(new_file_name);

                                // 只有当新旧路径不同时才进行重命名
                                if file_path != new_file_path {
                                    // **关键修复：检查目标文件是否已存在，避免覆盖**
                                    let final_new_file_path = if new_file_path.exists() {
                                        // 目标文件已存在，生成唯一文件名避免覆盖
                                        let file_stem = new_file_path.file_stem().unwrap_or_default().to_string_lossy();
                                        let file_extension =
                                            new_file_path.extension().unwrap_or_default().to_string_lossy();
                                        let parent_dir = new_file_path.parent().unwrap_or(&actual_file_dir);

                                        // 尝试添加BV号后缀避免冲突
                                        let bvid_suffix = &video.bvid;
                                        let unique_name = if file_extension.is_empty() {
                                            format!("{}-{}", file_stem, bvid_suffix)
                                        } else {
                                            format!("{}-{}.{}", file_stem, bvid_suffix, file_extension)
                                        };
                                        let unique_path = parent_dir.join(unique_name);

                                        // 如果BV号后缀仍然冲突，使用时间戳
                                        if unique_path.exists() {
                                            let timestamp = chrono::Local::now().format("%H%M%S").to_string();
                                            let final_name = if file_extension.is_empty() {
                                                format!("{}-{}-{}", file_stem, bvid_suffix, timestamp)
                                            } else {
                                                format!(
                                                    "{}-{}-{}.{}",
                                                    file_stem, bvid_suffix, timestamp, file_extension
                                                )
                                            };
                                            parent_dir.join(final_name)
                                        } else {
                                            unique_path
                                        }
                                    } else {
                                        new_file_path.clone()
                                    };

                                    match std::fs::rename(&file_path, &final_new_file_path) {
                                        Ok(_) => {
                                            if final_new_file_path != new_file_path {
                                                warn!(
                                                    "检测到文件名冲突，已重命名避免覆盖: {:?} -> {:?}",
                                                    file_path, final_new_file_path
                                                );
                                            } else {
                                                debug!(
                                                    "重命名分页相关文件成功: {:?} -> {:?}",
                                                    file_path, final_new_file_path
                                                );
                                            }
                                            updated_count += 1;

                                            // 如果这是主文件（MP4），更新数据库中的路径记录
                                            if file_name.ends_with(".mp4") {
                                                let new_path_str = final_new_file_path.to_string_lossy().to_string();
                                                let mut page_update: bili_sync_entity::page::ActiveModel =
                                                    page.clone().into();
                                                page_update.path = Set(Some(new_path_str));
                                                if let Err(e) = page_update.update(db.as_ref()).await {
                                                    warn!("更新数据库中的分页路径失败: {}", e);
                                                } else {
                                                    debug!("更新数据库分页路径成功: {:?}", final_new_file_path);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "重命名分页相关文件失败: {:?} -> {:?}, 错误: {}",
                                                file_path, final_new_file_path, e
                                            );
                                        }
                                    }
                                } else {
                                    debug!("文件路径已经正确，无需重命名: {:?}", file_path);
                                }
                            }
                        }
                    }
                }
            } else {
                debug!(
                    "分页 {} 的文件名已经是正确格式或未找到文件，原始模式: '{}', 新模式: '{}'",
                    page.pid, old_page_name, new_page_name
                );
            }
        }
    }

    info!("文件重命名完成，共处理了 {} 个文件/文件夹", updated_count);
    Ok(updated_count)
}

/// 获取番剧的所有季度信息
#[utoipa::path(
    get,
    path = "/api/bangumi/seasons/{season_id}",
    responses(
        (status = 200, body = ApiResponse<Vec<BangumiSeasonInfo>>),
    )
)]
pub async fn get_bangumi_seasons(
    Path(season_id): Path<String>,
) -> Result<ApiResponse<crate::api::response::BangumiSeasonsResponse>, ApiError> {
    use crate::bilibili::bangumi::Bangumi;
    use crate::bilibili::BiliClient;
    use futures::future::join_all;

    // 创建 BiliClient，使用空 cookie（对于获取季度信息不需要登录）
    let bili_client = BiliClient::new(String::new());

    // 创建 Bangumi 实例
    let bangumi = Bangumi::new(&bili_client, None, Some(season_id.clone()), None);

    // 获取所有季度信息
    match bangumi.get_all_seasons().await {
        Ok(seasons) => {
            // 并发获取所有季度的详细信息
            let season_details_futures: Vec<_> = seasons
                .iter()
                .map(|s| {
                    let bili_client_clone = bili_client.clone();
                    let season_clone = s.clone();
                    async move {
                        let season_bangumi = Bangumi::new(
                            &bili_client_clone,
                            season_clone.media_id.clone(),
                            Some(season_clone.season_id.clone()),
                            None,
                        );

                        let (full_title, episode_count, description) = match season_bangumi.get_season_info().await {
                            Ok(season_info) => {
                                let full_title = season_info["title"].as_str().map(|t| t.to_string());

                                // 获取集数信息
                                let episode_count =
                                    season_info["episodes"].as_array().map(|episodes| episodes.len() as i32);

                                // 获取简介信息
                                let description = season_info["evaluate"].as_str().map(|d| d.to_string());

                                (full_title, episode_count, description)
                            }
                            Err(e) => {
                                warn!("获取季度 {} 的详细信息失败: {}", season_clone.season_id, e);
                                (None, None, None)
                            }
                        };

                        (season_clone, full_title, episode_count, description)
                    }
                })
                .collect();

            // 等待所有并发请求完成
            let season_details = join_all(season_details_futures).await;

            // 构建响应数据
            let season_list: Vec<_> = season_details
                .into_iter()
                .map(
                    |(s, full_title, episode_count, description)| crate::api::response::BangumiSeasonInfo {
                        season_id: s.season_id,
                        season_title: s.season_title,
                        full_title,
                        media_id: s.media_id,
                        cover: Some(s.cover),
                        episode_count,
                        description,
                    },
                )
                .collect();

            Ok(ApiResponse::ok(crate::api::response::BangumiSeasonsResponse {
                success: true,
                data: season_list,
            }))
        }
        Err(e) => {
            error!("获取番剧季度信息失败: {}", e);
            Err(anyhow!("获取番剧季度信息失败: {}", e).into())
        }
    }
}

/// 搜索bilibili内容
#[utoipa::path(
    get,
    path = "/api/search",
    params(
        ("keyword" = String, Query, description = "搜索关键词"),
        ("search_type" = String, Query, description = "搜索类型：video, bili_user, media_bangumi"),
        ("page" = Option<u32>, Query, description = "页码，默认1"),
        ("page_size" = Option<u32>, Query, description = "每页数量，默认20")
    ),
    responses(
        (status = 200, body = ApiResponse<crate::api::response::SearchResponse>),
    )
)]
pub async fn search_bilibili(
    Query(params): Query<crate::api::request::SearchRequest>,
) -> Result<ApiResponse<crate::api::response::SearchResponse>, ApiError> {
    use crate::bilibili::{BiliClient, SearchResult};

    // 验证搜索类型
    let valid_types = ["video", "bili_user", "media_bangumi", "media_ft"];
    if !valid_types.contains(&params.search_type.as_str()) {
        return Err(anyhow!("不支持的搜索类型，支持的类型: {}", valid_types.join(", ")).into());
    }

    // 验证关键词
    if params.keyword.trim().is_empty() {
        return Err(anyhow!("搜索关键词不能为空").into());
    }

    // 创建 BiliClient，使用空 cookie（搜索不需要登录）
    let bili_client = BiliClient::new(String::new());

    // 特殊处理：当搜索类型为media_bangumi时，同时搜索番剧和影视
    let mut all_results = Vec::new();
    let mut total_results = 0u32;

    if params.search_type == "media_bangumi" {
        // 搜索番剧
        match bili_client
            .search(
                &params.keyword,
                "media_bangumi",
                params.page,
                params.page_size / 2, // 每种类型分配一半的结果数
            )
            .await
        {
            Ok(bangumi_wrapper) => {
                all_results.extend(bangumi_wrapper.results);
                total_results += bangumi_wrapper.total;
            }
            Err(e) => {
                warn!("搜索番剧失败: {}", e);
            }
        }

        // 搜索影视
        match bili_client
            .search(
                &params.keyword,
                "media_ft",
                params.page,
                params.page_size / 2, // 每种类型分配一半的结果数
            )
            .await
        {
            Ok(ft_wrapper) => {
                all_results.extend(ft_wrapper.results);
                total_results += ft_wrapper.total;
            }
            Err(e) => {
                warn!("搜索影视失败: {}", e);
            }
        }

        // 如果两个搜索都失败了，返回错误
        if all_results.is_empty() && total_results == 0 {
            return Err(anyhow!("搜索失败：无法获取番剧或影视结果").into());
        }
    } else {
        // 其他类型正常搜索
        match bili_client
            .search(&params.keyword, &params.search_type, params.page, params.page_size)
            .await
        {
            Ok(search_wrapper) => {
                all_results = search_wrapper.results;
                total_results = search_wrapper.total;
            }
            Err(e) => {
                error!("搜索失败: {}", e);
                return Err(anyhow!("搜索失败: {}", e).into());
            }
        }
    }

    // 转换搜索结果格式
    let api_results: Vec<crate::api::response::SearchResult> = all_results
        .into_iter()
        .map(|r: SearchResult| crate::api::response::SearchResult {
            result_type: r.result_type,
            title: r.title,
            author: r.author,
            bvid: r.bvid,
            aid: r.aid,
            mid: r.mid,
            season_id: r.season_id,
            media_id: r.media_id,
            cover: r.cover,
            description: r.description,
            duration: r.duration,
            pubdate: r.pubdate,
            play: r.play,
            danmaku: r.danmaku,
            follower: r.follower,
        })
        .collect();

    Ok(ApiResponse::ok(crate::api::response::SearchResponse {
        success: true,
        results: api_results,
        total: total_results,
        page: params.page,
        page_size: params.page_size,
    }))
}

/// 获取用户收藏夹列表
#[utoipa::path(
    get,
    path = "/api/user/favorites",
    responses(
        (status = 200, body = ApiResponse<Vec<crate::api::response::UserFavoriteFolder>>),
    )
)]
pub async fn get_user_favorites() -> Result<ApiResponse<Vec<crate::api::response::UserFavoriteFolder>>, ApiError> {
    let bili_client = crate::bilibili::BiliClient::new(String::new());

    match bili_client.get_user_favorite_folders(None).await {
        Ok(folders) => {
            let response_folders: Vec<crate::api::response::UserFavoriteFolder> = folders
                .into_iter()
                .map(|folder| crate::api::response::UserFavoriteFolder {
                    id: folder.id,
                    fid: folder.fid,
                    title: folder.title,
                    media_count: folder.media_count,
                })
                .collect();

            Ok(ApiResponse::ok(response_folders))
        }
        Err(e) => {
            error!("获取用户收藏夹列表失败: {}", e);
            Err(anyhow!("获取用户收藏夹列表失败: {}", e).into())
        }
    }
}

/// 获取UP主的合集和系列列表
#[utoipa::path(
    get,
    path = "/api/user/collections/{mid}",
    params(
        ("mid" = i64, Path, description = "UP主ID"),
        ("page" = Option<u32>, Query, description = "页码，默认1"),
        ("page_size" = Option<u32>, Query, description = "每页数量，默认20")
    ),
    responses(
        (status = 200, body = ApiResponse<crate::api::response::UserCollectionsResponse>),
    )
)]
pub async fn get_user_collections(
    Path(mid): Path<i64>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<ApiResponse<crate::api::response::UserCollectionsResponse>, ApiError> {
    let page = params.get("page").and_then(|p| p.parse::<u32>().ok()).unwrap_or(1);
    let page_size = params
        .get("page_size")
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(20);

    let bili_client = crate::bilibili::BiliClient::new(String::new());

    match bili_client.get_user_collections(mid, page, page_size).await {
        Ok(response) => Ok(ApiResponse::ok(response)),
        Err(e) => {
            let error_msg = format!("获取UP主 {} 的合集失败", mid);
            warn!("{}: {}", error_msg, e);

            // 检查是否是网络错误，提供更友好的错误信息
            let user_friendly_error =
                if e.to_string().contains("ERR_EMPTY_RESPONSE") || e.to_string().contains("Failed to fetch") {
                    "该UP主的合集可能需要登录访问，或暂时无法获取。请稍后重试或手动输入合集ID。".to_string()
                } else if e.to_string().contains("403") || e.to_string().contains("Forbidden") {
                    "该UP主的合集为私有，无法访问。".to_string()
                } else if e.to_string().contains("404") || e.to_string().contains("Not Found") {
                    "UP主不存在或合集已被删除。".to_string()
                } else {
                    "网络错误或服务暂时不可用，请稍后重试。".to_string()
                };

            Err(anyhow!("{}", user_friendly_error).into())
        }
    }
}

/// 计算目录大小的辅助函数
fn get_directory_size(path: &str) -> std::io::Result<u64> {
    fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
        let mut size = 0;
        if path.is_dir() {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    size += dir_size(&path)?;
                } else {
                    size += entry.metadata()?.len();
                }
            }
        }
        Ok(size)
    }

    let path = std::path::Path::new(path);
    dir_size(path)
}

/// 获取关注的UP主列表
#[utoipa::path(
    get,
    path = "/api/user/followings",
    responses(
        (status = 200, body = ApiResponse<Vec<crate::api::response::UserFollowing>>),
    )
)]
pub async fn get_user_followings() -> Result<ApiResponse<Vec<crate::api::response::UserFollowing>>, ApiError> {
    let bili_client = crate::bilibili::BiliClient::new(String::new());

    match bili_client.get_user_followings().await {
        Ok(followings) => {
            let response_followings: Vec<crate::api::response::UserFollowing> = followings
                .into_iter()
                .map(|following| crate::api::response::UserFollowing {
                    mid: following.mid,
                    name: following.name,
                    face: following.face,
                    sign: following.sign,
                    official_verify: following
                        .official_verify
                        .map(|verify| crate::api::response::OfficialVerify {
                            type_: verify.type_,
                            desc: verify.desc,
                        }),
                    follower: following.follower,
                })
                .collect();
            Ok(ApiResponse::ok(response_followings))
        }
        Err(e) => {
            tracing::error!("获取关注UP主列表失败: {}", e);
            Err(ApiError::from(anyhow::anyhow!("获取关注UP主列表失败: {}", e)))
        }
    }
}

/// 获取订阅的合集列表
#[utoipa::path(
    get,
    path = "/api/user/subscribed-collections",
    responses(
        (status = 200, body = ApiResponse<Vec<crate::api::response::UserCollectionInfo>>),
    )
)]
pub async fn get_subscribed_collections() -> Result<ApiResponse<Vec<crate::api::response::UserCollectionInfo>>, ApiError>
{
    let bili_client = crate::bilibili::BiliClient::new(String::new());

    match bili_client.get_subscribed_collections().await {
        Ok(collections) => Ok(ApiResponse::ok(collections)),
        Err(e) => {
            tracing::error!("获取订阅合集失败: {}", e);
            Err(ApiError::from(anyhow::anyhow!("获取订阅合集失败: {}", e)))
        }
    }
}

/// 获取UP主的历史投稿列表
#[utoipa::path(
    get,
    path = "/api/submission/{up_id}/videos",
    params(
        ("up_id" = String, Path, description = "UP主ID"),
        SubmissionVideosRequest,
    ),
    responses(
        (status = 200, body = ApiResponse<SubmissionVideosResponse>),
    )
)]
pub async fn get_submission_videos(
    Path(up_id): Path<String>,
    Query(params): Query<SubmissionVideosRequest>,
) -> Result<ApiResponse<SubmissionVideosResponse>, ApiError> {
    let bili_client = crate::bilibili::BiliClient::new(String::new());

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(50);

    // 验证UP主ID格式
    let up_id_i64 = up_id
        .parse::<i64>()
        .map_err(|_| ApiError::from(anyhow::anyhow!("无效的UP主ID格式")))?;

    // 获取UP主投稿列表（支持搜索关键词）
    let result = if let Some(keyword) = params.keyword.as_deref() {
        // 如果提供了关键词，使用搜索功能
        tracing::debug!("搜索UP主 {} 的视频，关键词: '{}'", up_id, keyword);
        bili_client
            .search_user_submission_videos(up_id_i64, keyword, page, page_size)
            .await
    } else {
        // 否则使用普通的获取功能
        bili_client.get_user_submission_videos(up_id_i64, page, page_size).await
    };

    match result {
        Ok((videos, total)) => {
            let response = SubmissionVideosResponse {
                videos,
                total,
                page,
                page_size,
            };

            Ok(ApiResponse::ok(response))
        }
        Err(e) => {
            tracing::error!("获取UP主 {} 投稿列表失败: {}", up_id, e);
            Err(ApiError::from(anyhow::anyhow!("获取UP主投稿列表失败: {}", e)))
        }
    }
}

/// 日志级别枚举
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, utoipa::ToSchema)]
pub enum LogLevel {
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "warn")]
    Warn,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "debug")]
    Debug,
}

/// 日志条目结构
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
    pub target: Option<String>,
}

/// 日志响应结构
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogsResponse {
    pub logs: Vec<LogEntry>,
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
    pub total_pages: usize,
}

// 全局日志存储，使用Arc<Mutex<VecDeque<LogEntry>>>来存储最近的日志
lazy_static::lazy_static! {
    static ref LOG_BUFFER: Arc<Mutex<VecDeque<LogEntry>>> = Arc::new(Mutex::new(VecDeque::with_capacity(100000)));
    // 为debug日志单独设置缓冲区，容量较小
    static ref DEBUG_LOG_BUFFER: Arc<Mutex<VecDeque<LogEntry>>> = Arc::new(Mutex::new(VecDeque::with_capacity(10000)));
    static ref LOG_BROADCASTER: broadcast::Sender<LogEntry> = {
        let (sender, _) = broadcast::channel(100);
        sender
    };
}

/// 添加日志到缓冲区
pub fn add_log_entry(level: LogLevel, message: String, target: Option<String>) {
    let entry = LogEntry {
        timestamp: now_standard_string(),
        level: level.clone(), // 克隆level避免所有权问题
        message,
        target,
    };

    match level {
        LogLevel::Debug => {
            // Debug日志使用单独的缓冲区，容量较小
            if let Ok(mut buffer) = DEBUG_LOG_BUFFER.lock() {
                buffer.push_back(entry.clone());
                // Debug日志保持在10000条以内
                if buffer.len() > 10000 {
                    buffer.pop_front();
                }
            }
        }
        _ => {
            // 其他级别日志使用主缓冲区
            if let Ok(mut buffer) = LOG_BUFFER.lock() {
                buffer.push_back(entry.clone());
                // 主缓冲区保持在50000条以内（给debug留出空间）
                if buffer.len() > 50000 {
                    buffer.pop_front();
                }
            }
        }
    }

    // 广播给实时订阅者
    let _ = LOG_BROADCASTER.send(entry);
}

/// 获取历史日志
#[utoipa::path(
    get,
    path = "/api/logs",
    params(
        ("level" = Option<String>, Query, description = "过滤日志级别: info, warn, error, debug"),
        ("limit" = Option<usize>, Query, description = "每页返回的日志数量，默认100，最大10000"),
        ("page" = Option<usize>, Query, description = "页码，从1开始，默认1")
    ),
    responses(
        (status = 200, description = "获取日志成功", body = LogsResponse),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn get_logs(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<ApiResponse<LogsResponse>, ApiError> {
    let level_filter = params.get("level").and_then(|l| match l.as_str() {
        "info" => Some(LogLevel::Info),
        "warn" => Some(LogLevel::Warn),
        "error" => Some(LogLevel::Error),
        "debug" => Some(LogLevel::Debug),
        _ => None,
    });

    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(100)
        .min(10000); // 提高最大限制到10000条

    let page = params
        .get("page")
        .and_then(|p| p.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1); // 页码最小为1

    let logs = if let Some(ref filter_level) = level_filter {
        if *filter_level == LogLevel::Debug {
            // 如果筛选debug级别，从debug专用缓冲区获取
            if let Ok(buffer) = DEBUG_LOG_BUFFER.lock() {
                let total_logs: Vec<LogEntry> = buffer
                    .iter()
                    .rev() // 最新的在前
                    .cloned()
                    .collect();

                let total = total_logs.len();
                let offset = (page - 1) * limit;
                let total_pages = if total == 0 { 0 } else { total.div_ceil(limit) };
                let logs = total_logs.into_iter().skip(offset).take(limit).collect();

                LogsResponse {
                    logs,
                    total,
                    page,
                    per_page: limit,
                    total_pages,
                }
            } else {
                LogsResponse {
                    logs: vec![],
                    total: 0,
                    page: 1,
                    per_page: limit,
                    total_pages: 0,
                }
            }
        } else {
            // 其他级别从主缓冲区获取
            if let Ok(buffer) = LOG_BUFFER.lock() {
                let total_logs: Vec<LogEntry> = buffer
                    .iter()
                    .rev() // 最新的在前
                    .filter(|entry| &entry.level == filter_level)
                    .cloned()
                    .collect();

                let total = total_logs.len();
                let offset = (page - 1) * limit;
                let total_pages = if total == 0 { 0 } else { total.div_ceil(limit) };
                let logs = total_logs.into_iter().skip(offset).take(limit).collect();

                LogsResponse {
                    logs,
                    total,
                    page,
                    per_page: limit,
                    total_pages,
                }
            } else {
                LogsResponse {
                    logs: vec![],
                    total: 0,
                    page: 1,
                    per_page: limit,
                    total_pages: 0,
                }
            }
        }
    } else {
        // 没有指定级别（全部日志），合并两个缓冲区但排除debug级别
        if let Ok(main_buffer) = LOG_BUFFER.lock() {
            let total_logs: Vec<LogEntry> = main_buffer
                .iter()
                .rev() // 最新的在前
                .filter(|entry| entry.level != LogLevel::Debug) // 排除debug级别
                .cloned()
                .collect();

            let total = total_logs.len();
            let offset = (page - 1) * limit;
            let total_pages = if total == 0 { 0 } else { total.div_ceil(limit) };
            let logs = total_logs.into_iter().skip(offset).take(limit).collect();

            LogsResponse {
                logs,
                total,
                page,
                per_page: limit,
                total_pages,
            }
        } else {
            LogsResponse {
                logs: vec![],
                total: 0,
                page: 1,
                per_page: limit,
                total_pages: 0,
            }
        }
    };

    Ok(ApiResponse::ok(logs))
}

/// 下载日志文件
#[utoipa::path(
    get,
    path = "/api/logs/download",
    params(
        ("level" = Option<String>, Query, description = "日志级别: all, info, warn, error, debug，默认all")
    ),
    responses(
        (status = 200, description = "下载日志文件成功"),
        (status = 404, description = "日志文件不存在"),
        (status = 500, description = "服务器内部错误")
    )
)]
pub async fn download_log_file(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    use axum::http::header;
    use tokio::fs;

    // 先刷新所有缓冲的日志到文件，确保下载的是最新的
    crate::utils::file_logger::flush_file_logger();

    // 获取日志级别参数
    let level = params.get("level").map(|s| s.as_str()).unwrap_or("all");

    // 允许指定具体文件名（便于排查历史轮次）
    let requested_file = params.get("file").cloned();

    // 构建日志文件路径
    let log_dir = crate::config::CONFIG_DIR.join("logs");
    let file_name = if let Some(file) = requested_file {
        file
    } else if let Some(file) = crate::utils::file_logger::get_current_log_file_name(level) {
        file
    } else {
        // 兜底：如果文件日志系统未初始化，尝试按“最新文件”选择
        let prefix = match level {
            "debug" => "logs-debug-",
            "info" => "logs-info-",
            "warn" => "logs-warn-",
            "error" => "logs-error-",
            _ => "logs-all-",
        };
        let latest = std::fs::read_dir(&log_dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if !file_name.starts_with(prefix) || !file_name.ends_with(".csv") {
                    return None;
                }
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Some((modified, file_name))
            })
            .max_by_key(|(modified, _)| *modified)
            .map(|(_, file_name)| file_name)
            .unwrap_or_else(|| "".to_string());

        if latest.is_empty() {
            return Err(InnerApiError::BadRequest("日志文件不存在".to_string()).into());
        }
        latest
    };

    let file_path = log_dir.join(&file_name);

    // 检查文件是否存在
    if !file_path.exists() {
        return Err(InnerApiError::BadRequest(format!("日志文件不存在: {}", file_name)).into());
    }

    // 读取文件内容
    let file_content = fs::read(&file_path)
        .await
        .map_err(|e| InnerApiError::BadRequest(format!("读取日志文件失败: {}", e)))?;

    // 构建响应
    let response = axum::response::Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", file_name),
        )
        .body(axum::body::Body::from(file_content))
        .map_err(|e| InnerApiError::BadRequest(format!("构建响应失败: {}", e)))?;

    Ok(response)
}

/// 获取可用的日志文件列表
#[utoipa::path(
    get,
    path = "/api/logs/files",
    responses(
        (status = 200, description = "获取日志文件列表成功", body = LogFilesResponse),
        (status = 500, description = "服务器内部错误")
    )
)]
pub async fn get_log_files() -> Result<ApiResponse<LogFilesResponse>, ApiError> {
    use std::fs;

    let log_dir = crate::config::CONFIG_DIR.join("logs");
    let mut files = vec![];

    // 列出所有日志文件（每轮会生成新文件）
    if let Ok(entries) = fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let level = if file_name.starts_with("logs-all-") {
                "all"
            } else if file_name.starts_with("logs-debug-") {
                "debug"
            } else if file_name.starts_with("logs-info-") {
                "info"
            } else if file_name.starts_with("logs-warn-") {
                "warn"
            } else if file_name.starts_with("logs-error-") {
                "error"
            } else {
                continue;
            };

            if !file_name.ends_with(".csv") {
                continue;
            }

            if let Ok(metadata) = entry.metadata() {
                files.push(LogFileInfo {
                    level: level.to_string(),
                    file_name,
                    size: metadata.len(),
                    modified: metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                });
            }
        }
    }

    // 最新的放前面
    files.sort_by(|a, b| b.modified.cmp(&a.modified));

    Ok(ApiResponse::ok(LogFilesResponse { files }))
}

/// 日志文件信息
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogFileInfo {
    pub level: String,
    pub file_name: String,
    pub size: u64,
    pub modified: u64,
}

/// 日志文件列表响应
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogFilesResponse {
    pub files: Vec<LogFileInfo>,
}

/// 队列任务信息结构体
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueueTaskInfo {
    pub task_id: String,
    pub task_type: String,
    pub description: String,
    pub created_at: String,
}

/// 队列状态响应结构体
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueueStatusResponse {
    pub is_scanning: bool,
    pub delete_queue: QueueInfo,
    pub video_delete_queue: QueueInfo,
    pub add_queue: QueueInfo,
    pub config_queue: ConfigQueueInfo,
}

/// 队列信息结构体
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct QueueInfo {
    pub length: usize,
    pub is_processing: bool,
    pub tasks: Vec<QueueTaskInfo>,
}

/// 配置队列信息结构体
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ConfigQueueInfo {
    pub update_length: usize,
    pub reload_length: usize,
    pub is_processing: bool,
    pub update_tasks: Vec<QueueTaskInfo>,
    pub reload_tasks: Vec<QueueTaskInfo>,
}

/// 取消队列任务响应结构体
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CancelQueueTaskResponse {
    pub success: bool,
    pub task_id: String,
    pub message: String,
}

/// 获取队列状态
#[utoipa::path(
    get,
    path = "/api/queue/status",
    responses(
        (status = 200, description = "获取队列状态成功", body = QueueStatusResponse),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn get_queue_status() -> Result<ApiResponse<QueueStatusResponse>, ApiError> {
    use crate::task::{ADD_TASK_QUEUE, CONFIG_TASK_QUEUE, DELETE_TASK_QUEUE, TASK_CONTROLLER, VIDEO_DELETE_TASK_QUEUE};

    // 获取扫描状态
    let is_scanning = TASK_CONTROLLER.is_scanning();

    // 获取删除队列状态
    let delete_raw_tasks = DELETE_TASK_QUEUE.list_tasks().await;
    let delete_queue_length = delete_raw_tasks.len();
    let delete_is_processing = DELETE_TASK_QUEUE.is_processing();
    let delete_tasks = delete_raw_tasks
        .into_iter()
        .map(|task| QueueTaskInfo {
            task_id: task.task_id,
            task_type: "delete_video_source".to_string(),
            description: format!("删除视频源 {}:{}", task.source_type, task.source_id),
            created_at: now_standard_string(),
        })
        .collect();

    // 获取视频删除队列状态
    let video_delete_raw_tasks = VIDEO_DELETE_TASK_QUEUE.list_tasks().await;
    let video_delete_queue_length = video_delete_raw_tasks.len();
    let video_delete_is_processing = VIDEO_DELETE_TASK_QUEUE.is_processing();

    let video_delete_tasks = video_delete_raw_tasks
        .into_iter()
        .map(|task| QueueTaskInfo {
            task_id: task.task_id,
            task_type: "delete_video".to_string(),
            description: format!("删除视频 ID={}", task.video_id),
            created_at: now_standard_string(),
        })
        .collect();

    // 获取添加队列状态
    let add_raw_tasks = ADD_TASK_QUEUE.list_tasks().await;
    let add_queue_length = add_raw_tasks.len();
    let add_is_processing = ADD_TASK_QUEUE.is_processing();

    let add_tasks = add_raw_tasks
        .into_iter()
        .map(|task| QueueTaskInfo {
            task_id: task.task_id,
            task_type: "add_video_source".to_string(),
            description: format!("添加视频源 {}", task.name),
            created_at: now_standard_string(),
        })
        .collect();

    // 获取配置队列状态
    let config_update_raw_tasks = CONFIG_TASK_QUEUE.list_update_tasks().await;
    let config_reload_raw_tasks = CONFIG_TASK_QUEUE.list_reload_tasks().await;
    let config_update_length = config_update_raw_tasks.len();
    let config_reload_length = config_reload_raw_tasks.len();
    let config_is_processing = CONFIG_TASK_QUEUE.is_processing();

    let config_update_tasks = config_update_raw_tasks
        .into_iter()
        .map(|task| QueueTaskInfo {
            task_id: task.task_id,
            task_type: "update_config".to_string(),
            description: "更新配置任务".to_string(),
            created_at: now_standard_string(),
        })
        .collect();

    let config_reload_tasks = config_reload_raw_tasks
        .into_iter()
        .map(|task| QueueTaskInfo {
            task_id: task.task_id,
            task_type: "reload_config".to_string(),
            description: "重载配置任务".to_string(),
            created_at: now_standard_string(),
        })
        .collect();

    let response = QueueStatusResponse {
        is_scanning,
        delete_queue: QueueInfo {
            length: delete_queue_length,
            is_processing: delete_is_processing,
            tasks: delete_tasks,
        },
        video_delete_queue: QueueInfo {
            length: video_delete_queue_length,
            is_processing: video_delete_is_processing,
            tasks: video_delete_tasks,
        },
        add_queue: QueueInfo {
            length: add_queue_length,
            is_processing: add_is_processing,
            tasks: add_tasks,
        },
        config_queue: ConfigQueueInfo {
            update_length: config_update_length,
            reload_length: config_reload_length,
            is_processing: config_is_processing,
            update_tasks: config_update_tasks,
            reload_tasks: config_reload_tasks,
        },
    };

    Ok(ApiResponse::ok(response))
}

/// 取消队列中的待处理任务
#[utoipa::path(
    delete,
    path = "/api/queue/tasks/{task_id}",
    params(
        ("task_id" = String, Path, description = "任务ID")
    ),
    responses(
        (status = 200, description = "取消任务成功", body = CancelQueueTaskResponse),
        (status = 400, description = "任务不存在或已进入处理", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn cancel_queue_task(
    Path(task_id): Path<String>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<CancelQueueTaskResponse>, ApiError> {
    let task_id = task_id.trim().to_string();
    if task_id.is_empty() {
        return Err(InnerApiError::BadRequest("任务ID不能为空".to_string()).into());
    }

    let cancelled = crate::task::cancel_pending_task(&task_id, &db).await?;
    if !cancelled {
        return Err(InnerApiError::BadRequest("任务不存在、已取消或已进入处理中".to_string()).into());
    }

    Ok(ApiResponse::ok(CancelQueueTaskResponse {
        success: true,
        task_id: task_id.clone(),
        message: format!("任务 {} 已取消", task_id),
    }))
}

/// 代理B站图片请求，解决防盗链问题
fn proxy_image_cache_key(url: &str) -> String {
    format!("{:x}", md5::compute(url.as_bytes()))
}

fn summarize_image_url(url: &str) -> String {
    let mut parts = url.splitn(2, '?');
    let base = parts.next().unwrap_or_default();
    let query_len = parts.next().map(|q| q.len()).unwrap_or(0);
    if query_len > 0 {
        format!("{base} (query_len={query_len})")
    } else {
        base.to_string()
    }
}

fn proxy_image_etag(image_data: &[u8]) -> String {
    format!("\"{:x}\"", md5::compute(image_data))
}

fn if_none_match_hit(headers: &HeaderMap, etag: &str) -> bool {
    let Some(raw) = headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok()) else {
        return false;
    };

    raw.split(',')
        .map(|item| item.trim())
        .any(|tag| tag == "*" || tag == etag)
}

async fn maybe_cleanup_proxy_image_cache(db: &DatabaseConnection, now: DateTime<Utc>) {
    {
        let mut last_cleanup = IMAGE_PROXY_CACHE_LAST_CLEANUP_AT.write().await;
        if let Some(last) = *last_cleanup {
            let since_last = (now - last).num_seconds();
            if since_last < IMAGE_PROXY_CACHE_CLEANUP_INTERVAL_SECONDS {
                debug!(
                    "图片数据库缓存清理跳过: now={}, last={}, since_last={}s, interval={}s",
                    now.timestamp(),
                    last.timestamp(),
                    since_last,
                    IMAGE_PROXY_CACHE_CLEANUP_INTERVAL_SECONDS
                );
                return;
            }
        }
        *last_cleanup = Some(now);
    }

    let cleanup_sql = r#"
        DELETE FROM image_proxy_cache
        WHERE expires_at_unix <= ?
    "#;
    let backend = db.get_database_backend();
    let result = db
        .execute(Statement::from_sql_and_values(
            backend,
            cleanup_sql,
            vec![now.timestamp().into()],
        ))
        .await;

    match result {
        Ok(res) => {
            let rows = res.rows_affected();
            debug!("图片数据库缓存清理完成: cleaned_rows={}, now={}", rows, now.timestamp());
        }
        Err(e) => {
            debug!("图片数据库缓存清理失败（不影响请求）: {}", e);
        }
    }
}

async fn load_proxy_image_cache(
    db: &DatabaseConnection,
    url: &str,
    now: DateTime<Utc>,
) -> Option<(String, Vec<u8>, String)> {
    let cache_key = proxy_image_cache_key(url);
    let select_sql = r#"
        SELECT content_type, image_data, etag, expires_at_unix
        FROM image_proxy_cache
        WHERE cache_key = ?
        LIMIT 1
    "#;
    let backend = db.get_database_backend();
    debug!("图片缓存查询开始: key={}, url={}", cache_key, summarize_image_url(url));

    let row = match db
        .query_one(Statement::from_sql_and_values(
            backend,
            select_sql,
            vec![cache_key.clone().into()],
        ))
        .await
    {
        Ok(row) => row,
        Err(e) => {
            debug!(
                "图片缓存查询失败（按未命中处理）: key={}, url={}, error={}",
                cache_key,
                summarize_image_url(url),
                e
            );
            return None;
        }
    };

    let Some(row) = row else {
        debug!("图片缓存未命中: key={}, url={}", cache_key, summarize_image_url(url));
        return None;
    };

    let content_type = match row.try_get_by_index::<String>(0) {
        Ok(v) => v,
        Err(e) => {
            debug!("图片缓存解析失败(content_type): key={}, error={}", cache_key, e);
            return None;
        }
    };
    let image_data = match row.try_get_by_index::<Vec<u8>>(1) {
        Ok(v) => v,
        Err(e) => {
            debug!("图片缓存解析失败(image_data): key={}, error={}", cache_key, e);
            return None;
        }
    };
    let etag = match row.try_get_by_index::<String>(2) {
        Ok(v) => v,
        Err(e) => {
            debug!("图片缓存解析失败(etag): key={}, error={}", cache_key, e);
            return None;
        }
    };
    let expires_at_unix = match row.try_get_by_index::<i64>(3) {
        Ok(v) => v,
        Err(e) => {
            debug!("图片缓存解析失败(expires_at_unix): key={}, error={}", cache_key, e);
            return None;
        }
    };

    if expires_at_unix <= now.timestamp() {
        debug!(
            "图片缓存已过期: key={}, url={}, expires_at={}, now={}",
            cache_key,
            summarize_image_url(url),
            expires_at_unix,
            now.timestamp()
        );
        let delete_sql = "DELETE FROM image_proxy_cache WHERE cache_key = ?";
        if let Err(e) = db
            .execute(Statement::from_sql_and_values(
                backend,
                delete_sql,
                vec![cache_key.into()],
            ))
            .await
        {
            debug!("图片缓存过期删除失败（不影响继续回源）: error={}", e);
        }
        return None;
    }

    debug!(
        "图片缓存命中: key={}, url={}, content_type={}, bytes={}, expires_at={}",
        cache_key,
        summarize_image_url(url),
        content_type,
        image_data.len(),
        expires_at_unix
    );

    Some((content_type, image_data, etag))
}

async fn store_proxy_image_cache(
    db: &DatabaseConnection,
    url: &str,
    content_type: &str,
    image_data: &[u8],
    etag: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let cache_key = proxy_image_cache_key(url);
    let now_unix = now.timestamp();
    let expires_at_unix = now_unix + IMAGE_PROXY_CACHE_TTL_SECONDS;
    let upsert_sql = r#"
        INSERT INTO image_proxy_cache (
            cache_key,
            url,
            content_type,
            image_data,
            etag,
            cached_at_unix,
            expires_at_unix,
            updated_at_unix
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(cache_key) DO UPDATE SET
            url = excluded.url,
            content_type = excluded.content_type,
            image_data = excluded.image_data,
            etag = excluded.etag,
            cached_at_unix = excluded.cached_at_unix,
            expires_at_unix = excluded.expires_at_unix,
            updated_at_unix = excluded.updated_at_unix
    "#;

    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        upsert_sql,
        vec![
            cache_key.clone().into(),
            url.to_string().into(),
            content_type.to_string().into(),
            image_data.to_vec().into(),
            etag.to_string().into(),
            now_unix.into(),
            expires_at_unix.into(),
            now_unix.into(),
        ],
    ))
    .await
    .context("写入图片数据库缓存失败")?;

    debug!(
        "图片缓存写入成功: key={}, url={}, content_type={}, bytes={}, ttl_seconds={}, expires_at={}",
        cache_key,
        summarize_image_url(url),
        content_type,
        image_data.len(),
        IMAGE_PROXY_CACHE_TTL_SECONDS,
        expires_at_unix
    );

    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/proxy/image",
    params(
        ("url" = String, Query, description = "图片URL"),
    ),
    responses(
        (status = 200, description = "图片数据", content_type = "image/*"),
        (status = 400, description = "无效的URL"),
        (status = 404, description = "图片不存在"),
    )
)]
pub async fn proxy_image(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, ApiError> {
    let url = params
        .get("url")
        .ok_or_else(|| anyhow!("缺少url参数"))?
        .trim()
        .to_string();
    debug!(
        "图片代理请求开始: url={}, if_none_match_present={}",
        summarize_image_url(&url),
        headers.get(IF_NONE_MATCH).is_some()
    );

    // 验证URL是否来自B站
    if !url.contains("hdslb.com") && !url.contains("bilibili.com") {
        debug!("图片代理拒绝非B站URL: {}", summarize_image_url(&url));
        return Err(anyhow!("只支持B站图片URL").into());
    }

    let now = Utc::now();
    maybe_cleanup_proxy_image_cache(db.as_ref(), now).await;

    if let Some((cached_content_type, image_data, cached_etag)) = load_proxy_image_cache(db.as_ref(), &url, now).await {
        if if_none_match_hit(&headers, &cached_etag) {
            debug!(
                "图片代理返回304(缓存命中): url={}, etag={}",
                summarize_image_url(&url),
                cached_etag
            );
            return Ok(axum::response::Response::builder()
                .status(304)
                .header("ETag", cached_etag)
                .header("Cache-Control", IMAGE_PROXY_CACHE_CONTROL)
                .header("X-Image-Cache", "HIT")
                .body(axum::body::Body::empty())
                .unwrap());
        }

        debug!(
            "图片代理返回200(缓存命中): url={}, content_type={}, bytes={}",
            summarize_image_url(&url),
            cached_content_type,
            image_data.len()
        );
        return Ok(axum::response::Response::builder()
            .status(200)
            .header("Content-Type", cached_content_type)
            .header("ETag", cached_etag)
            .header("Cache-Control", IMAGE_PROXY_CACHE_CONTROL)
            .header("X-Image-Cache", "HIT")
            .body(axum::body::Body::from(image_data))
            .unwrap());
    }

    // 创建HTTP客户端
    let client = reqwest::Client::new();

    // 请求图片，添加必要的请求头
    tracing::debug!("图片缓存未命中，开始回源下载: {}", summarize_image_url(&url));

    let request = client.get(&url).headers(create_image_headers());

    // 图片下载请求头日志已在建造器时设置

    let response = request.send().await;
    let response = match response {
        Ok(resp) => {
            tracing::debug!("图片下载请求成功 - 状态码: {}, URL: {}", resp.status(), resp.url());
            resp
        }
        Err(e) => {
            tracing::error!("图片下载请求失败 - URL: {}, 错误: {}", url, e);
            return Err(anyhow!("请求图片失败: {}", e).into());
        }
    };

    if !response.status().is_success() {
        tracing::error!("图片下载状态码错误 - URL: {}, 状态码: {}", url, response.status());
        return Err(anyhow!("图片请求失败: {}", response.status()).into());
    }

    // 获取内容类型
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    // 获取图片数据
    let image_data = response.bytes().await.map_err(|e| anyhow!("读取图片数据失败: {}", e))?;
    let etag = proxy_image_etag(&image_data);
    debug!(
        "图片回源下载完成: url={}, content_type={}, bytes={}, etag={}",
        summarize_image_url(&url),
        content_type,
        image_data.len(),
        etag
    );

    if if_none_match_hit(&headers, &etag) {
        debug!(
            "图片代理返回304(回源后命中If-None-Match): url={}, etag={}",
            summarize_image_url(&url),
            etag
        );
        return Ok(axum::response::Response::builder()
            .status(304)
            .header("ETag", etag)
            .header("Cache-Control", IMAGE_PROXY_CACHE_CONTROL)
            .header("X-Image-Cache", "MISS")
            .body(axum::body::Body::empty())
            .unwrap());
    }

    if let Err(e) = store_proxy_image_cache(db.as_ref(), &url, &content_type, &image_data, &etag, now).await {
        debug!("写入图片缓存失败（不影响返回）: url={}, error={}", url, e);
    }

    debug!(
        "图片代理返回200(回源): url={}, content_type={}, bytes={}",
        summarize_image_url(&url),
        content_type,
        image_data.len()
    );

    // 返回图片响应
    Ok(axum::response::Response::builder()
        .status(200)
        .header("Content-Type", content_type.as_str())
        .header("ETag", etag)
        .header("Cache-Control", IMAGE_PROXY_CACHE_CONTROL)
        .header("X-Image-Cache", "MISS")
        .body(axum::body::Body::from(image_data))
        .unwrap())
}

// ============================================================================
// 配置管理 API 端点
// ============================================================================

/// 获取单个配置项
#[utoipa::path(
    get,
    path = "/api/config/item/{key}",
    responses(
        (status = 200, description = "成功获取配置项", body = ConfigItemResponse),
        (status = 404, description = "配置项不存在"),
        (status = 500, description = "内部服务器错误")
    ),
    security(("Token" = []))
)]
pub async fn get_config_item(
    Path(key): Path<String>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<ConfigItemResponse>, ApiError> {
    use bili_sync_entity::entities::{config_item, prelude::ConfigItem};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    // 从数据库查找配置项
    let config_item = ConfigItem::find()
        .filter(config_item::Column::KeyName.eq(&key))
        .one(db.as_ref())
        .await
        .map_err(|e| ApiError::from(anyhow!("查询配置项失败: {}", e)))?;

    match config_item {
        Some(item) => {
            let value: serde_json::Value =
                serde_json::from_str(&item.value_json).map_err(|e| ApiError::from(anyhow!("解析配置值失败: {}", e)))?;

            let response = ConfigItemResponse {
                key: item.key_name,
                value,
                updated_at: item.updated_at,
            };

            Ok(ApiResponse::ok(response))
        }
        None => {
            use crate::api::error::InnerApiError;
            Err(ApiError::from(InnerApiError::BadRequest(format!(
                "配置项 '{}' 不存在",
                key
            ))))
        }
    }
}

// 删除未使用的外层函数，保留内部实现

pub async fn update_config_item_internal(
    db: Arc<DatabaseConnection>,
    key: String,
    request: UpdateConfigItemRequest,
) -> Result<ConfigItemResponse, ApiError> {
    use crate::config::ConfigManager;

    // 创建配置管理器
    let manager = ConfigManager::new(db.as_ref().clone());

    // 更新配置项
    if let Err(e) = manager.update_config_item(&key, request.value.clone()).await {
        warn!("更新配置项失败: {}", e);
        return Err(ApiError::from(anyhow!("更新配置项失败: {}", e)));
    }

    // 重新加载配置包
    if let Err(e) = crate::config::reload_config_bundle().await {
        warn!("重新加载配置包失败: {}", e);
    }

    // 返回响应
    let response = ConfigItemResponse {
        key: key.clone(),
        value: request.value,
        updated_at: now_standard_string(),
    };

    Ok(response)
}

// 删除未使用的外层函数，保留内部实现

pub async fn batch_update_config_internal(
    db: Arc<DatabaseConnection>,
    request: BatchUpdateConfigRequest,
) -> Result<ConfigReloadResponse, ApiError> {
    use crate::config::ConfigManager;

    let manager = ConfigManager::new(db.as_ref().clone());

    // 批量更新配置项
    for (key, value) in request.items {
        if let Err(e) = manager.update_config_item(&key, value).await {
            warn!("更新配置项 '{}' 失败: {}", key, e);
            return Err(ApiError::from(anyhow!("更新配置项 '{}' 失败: {}", key, e)));
        }
    }

    // 重新加载配置包
    if let Err(e) = crate::config::reload_config_bundle().await {
        warn!("重新加载配置包失败: {}", e);
        return Err(ApiError::from(anyhow!("重新加载配置包失败: {}", e)));
    }

    let response = ConfigReloadResponse {
        success: true,
        message: "配置批量更新成功".to_string(),
        reloaded_at: now_standard_string(),
    };

    Ok(response)
}

// 删除未使用的外层函数，保留内部实现

pub async fn reload_config_new_internal(_db: Arc<DatabaseConnection>) -> Result<ConfigReloadResponse, ApiError> {
    // 重新加载配置包
    if let Err(e) = crate::config::reload_config_bundle().await {
        warn!("重新加载配置包失败: {}", e);
        return Err(ApiError::from(anyhow!("重新加载配置包失败: {}", e)));
    }

    let response = ConfigReloadResponse {
        success: true,
        message: "配置重载成功".to_string(),
        reloaded_at: now_standard_string(),
    };

    Ok(response)
}

/// 获取配置变更历史
#[utoipa::path(
    get,
    path = "/api/config/history",
    params(ConfigHistoryRequest),
    responses(
        (status = 200, description = "成功获取配置变更历史", body = ConfigHistoryResponse),
        (status = 500, description = "内部服务器错误")
    ),
    security(("Token" = []))
)]
pub async fn get_config_history(
    Query(params): Query<ConfigHistoryRequest>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<ConfigHistoryResponse>, ApiError> {
    use crate::config::ConfigManager;

    let manager = ConfigManager::new(db.as_ref().clone());

    let changes = manager
        .get_config_history(params.key.as_deref(), params.limit)
        .await
        .map_err(|e| ApiError::from(anyhow!("获取配置变更历史失败: {}", e)))?;

    let change_infos: Vec<ConfigChangeInfo> = changes
        .into_iter()
        .map(|change| ConfigChangeInfo {
            id: change.id,
            key_name: change.key_name,
            old_value: change.old_value,
            new_value: change.new_value,
            changed_at: change.changed_at,
        })
        .collect();

    let response = ConfigHistoryResponse {
        total: change_infos.len(),
        changes: change_infos,
    };

    Ok(ApiResponse::ok(response))
}

/// 获取配置迁移状态
#[utoipa::path(
    get,
    path = "/api/config/migration/status",
    responses(
        (status = 200, description = "成功获取迁移状态", body = ConfigMigrationStatusResponse),
        (status = 500, description = "内部服务器错误")
    ),
    security(("Token" = []))
)]
pub async fn get_config_migration_status(
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<ConfigMigrationStatusResponse>, ApiError> {
    let manager = crate::config::ConfigManager::new(db.as_ref().clone());
    let status = manager
        .get_config_schema_status()
        .await
        .map_err(|e| ApiError::from(anyhow!("获取配置迁移状态失败: {}", e)))?;

    Ok(ApiResponse::ok(ConfigMigrationStatusResponse {
        current_version: status.current_version,
        latest_version: status.latest_version,
        pending: status.pending,
        legacy_detected: status.legacy_detected,
        last_migrated_at: status.last_migrated_at,
    }))
}

/// 执行配置迁移
#[utoipa::path(
    post,
    path = "/api/config/migrate",
    request_body = ConfigMigrationRequest,
    responses(
        (status = 200, description = "迁移结果", body = ConfigMigrationReportResponse),
        (status = 500, description = "内部服务器错误")
    ),
    security(("Token" = []))
)]
pub async fn migrate_config_schema(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Json(request): Json<ConfigMigrationRequest>,
) -> Result<ApiResponse<ConfigMigrationReportResponse>, ApiError> {
    let manager = crate::config::ConfigManager::new(db.as_ref().clone());
    let dry_run = request.dry_run.unwrap_or(false);
    let report = manager
        .migrate_config_schema(dry_run)
        .await
        .map_err(|e| ApiError::from(anyhow!("配置迁移失败: {}", e)))?;

    if !dry_run {
        if let Err(e) = crate::config::reload_config_bundle().await {
            warn!("迁移后重新加载配置失败: {}", e);
        }
    }

    Ok(ApiResponse::ok(ConfigMigrationReportResponse {
        current_version: report.current_version,
        target_version: report.target_version,
        applied: report.applied,
        dry_run: report.dry_run,
        legacy_detected: report.legacy_detected,
        mapped_keys: report.mapped_keys,
        unmapped_keys: report.unmapped_keys,
        notes: report.notes,
    }))
}

/// 验证配置
#[utoipa::path(
    post,
    path = "/api/config/validate",
    responses(
        (status = 200, description = "配置验证结果", body = ConfigValidationResponse),
        (status = 500, description = "内部服务器错误")
    ),
    security(("Token" = []))
)]
pub async fn validate_config(
    Extension(_db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<ConfigValidationResponse>, ApiError> {
    // 使用当前配置进行验证
    let is_valid = crate::config::with_config(|bundle| bundle.validate());

    let response = ConfigValidationResponse {
        valid: is_valid,
        errors: if is_valid {
            vec![]
        } else {
            vec!["配置验证失败".to_string()]
        },
        warnings: vec![],
    };

    Ok(ApiResponse::ok(response))
}

/// 获取热重载状态
#[utoipa::path(
    get,
    path = "/api/config/hot-reload/status",
    responses(
        (status = 200, description = "热重载状态", body = HotReloadStatusResponse),
        (status = 500, description = "内部服务器错误")
    ),
    security(("Token" = []))
)]
pub async fn get_hot_reload_status(
    Extension(_db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<HotReloadStatusResponse>, ApiError> {
    // TODO: 实现真正的热重载状态检查
    let response = HotReloadStatusResponse {
        enabled: true,
        last_reload: Some(now_standard_string()),
        pending_changes: 0,
    };

    Ok(ApiResponse::ok(response))
}

/// 检查是否需要初始设置
#[utoipa::path(
    get,
    path = "/api/setup/check",
    responses(
        (status = 200, description = "初始设置检查结果", body = InitialSetupCheckResponse),
        (status = 500, description = "内部服务器错误")
    )
)]
pub async fn check_initial_setup() -> Result<ApiResponse<InitialSetupCheckResponse>, ApiError> {
    // 使用配置包系统获取最新配置
    let (has_auth_token, has_credential) = crate::config::with_config(|bundle| {
        let config = &bundle.config;

        // 检查是否有auth_token
        let has_auth_token = config.auth_token.is_some() && !config.auth_token.as_ref().unwrap().is_empty();

        // 检查是否有凭证
        let credential = config.credential.load();
        let has_credential = match credential.as_deref() {
            Some(cred) => {
                !cred.sessdata.is_empty()
                    && !cred.bili_jct.is_empty()
                    && !cred.buvid3.is_empty()
                    && !cred.dedeuserid.is_empty()
            }
            None => false,
        };

        (has_auth_token, has_credential)
    });

    // 如果没有auth_token，则需要初始设置
    let needs_setup = !has_auth_token;

    let response = InitialSetupCheckResponse {
        needs_setup,
        has_auth_token,
        has_credential,
    };

    Ok(ApiResponse::ok(response))
}

/// 设置API Token（初始设置）
#[utoipa::path(
    post,
    path = "/api/setup/auth-token",
    request_body = SetupAuthTokenRequest,
    responses(
        (status = 200, description = "API Token设置成功", body = SetupAuthTokenResponse),
        (status = 400, description = "请求参数错误", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn setup_auth_token(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(params): axum::Json<crate::api::request::SetupAuthTokenRequest>,
) -> Result<ApiResponse<crate::api::response::SetupAuthTokenResponse>, ApiError> {
    if params.auth_token.trim().is_empty() {
        return Err(ApiError::from(anyhow!("API Token不能为空")));
    }

    // 更新配置中的auth_token
    let mut config = crate::config::reload_config();
    config.auth_token = Some(params.auth_token.clone());

    // 移除配置文件保存 - 配置现在完全基于数据库
    // config.save().map_err(|e| ApiError::from(anyhow!("保存配置失败: {}", e)))?;

    // 检查是否正在扫描，如果是则通过任务队列处理
    if crate::task::is_scanning() {
        // 将配置更新任务加入队列
        use uuid::Uuid;
        let reload_task = crate::task::ReloadConfigTask {
            task_id: Uuid::new_v4().to_string(),
        };
        crate::task::enqueue_reload_task(reload_task, &db).await?;
        info!("检测到正在扫描，API Token保存任务已加入队列");
    } else {
        // 只更新 API Token 配置项，避免覆盖其他配置
        use crate::config::ConfigManager;
        let manager = ConfigManager::new(db.as_ref().clone());

        let auth_token_json = serde_json::to_value(&config.auth_token).map_err(|e| {
            warn!("序列化API Token失败: {}", e);
            e
        });

        if let Ok(token_value) = auth_token_json {
            if let Err(e) = manager.update_config_item("auth_token", token_value).await {
                warn!("更新API Token配置失败: {}", e);
            } else {
                info!("API Token已保存到数据库");
            }
        }

        // 重新加载全局配置包（从数据库）
        if let Err(e) = crate::config::reload_config_bundle().await {
            warn!("重新加载配置包失败: {}", e);
            // 回退到传统的重新加载方式
            crate::config::reload_config();
        }
    }

    let response = crate::api::response::SetupAuthTokenResponse {
        success: true,
        message: "API Token设置成功".to_string(),
    };

    Ok(ApiResponse::ok(response))
}

/// 更新B站登录凭证
#[utoipa::path(
    put,
    path = "/api/credential",
    request_body = UpdateCredentialRequest,
    responses(
        (status = 200, description = "凭证更新成功", body = UpdateCredentialResponse),
        (status = 400, description = "请求参数错误", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn update_credential(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(params): axum::Json<crate::api::request::UpdateCredentialRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateCredentialResponse>, ApiError> {
    // 验证必填字段
    if params.sessdata.trim().is_empty()
        || params.bili_jct.trim().is_empty()
        || params.buvid3.trim().is_empty()
        || params.dedeuserid.trim().is_empty()
    {
        return Err(ApiError::from(anyhow!("请填写所有必需的凭证信息")));
    }

    // 创建新的凭证
    let mut new_credential = crate::bilibili::Credential {
        sessdata: params.sessdata.trim().to_string(),
        bili_jct: params.bili_jct.trim().to_string(),
        buvid3: params.buvid3.trim().to_string(),
        dedeuserid: params.dedeuserid.trim().to_string(),
        ac_time_value: params.ac_time_value.unwrap_or_default().trim().to_string(),
        buvid4: params
            .buvid4
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        dedeuserid_ckmd5: params
            .dedeuserid_ckmd5
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    };

    // 如果用户没有提供 buvid4，尝试通过 spi 接口获取
    if new_credential.buvid4.is_none() {
        if let Ok(client) = reqwest::Client::new()
            .get("https://api.bilibili.com/x/frontend/finger/spi")
            .header("Referer", "https://www.bilibili.com")
            .header("Origin", "https://www.bilibili.com")
            .send()
            .await
        {
            if let Ok(data) = client.json::<serde_json::Value>().await {
                if data["code"].as_i64() == Some(0) {
                    if let Some(buvid4) = data["data"]["b_4"].as_str() {
                        new_credential.buvid4 = Some(buvid4.to_string());
                        tracing::debug!("通过 spi 接口获取到 buvid4: {}", buvid4);
                    } else {
                        tracing::warn!("spi 接口未返回 buvid4");
                    }
                }
            }
        }
    } else {
        tracing::debug!("使用用户提供的 buvid4");
    }

    // 记录 dedeuserid_ckmd5 的来源
    if new_credential.dedeuserid_ckmd5.is_some() {
        tracing::debug!("使用用户提供的 DedeUserID__ckMd5");
    }

    // 更新配置中的凭证
    let config = crate::config::reload_config();
    config.credential.store(Some(std::sync::Arc::new(new_credential)));

    // 移除配置文件保存 - 配置现在完全基于数据库
    // config.save().map_err(|e| ApiError::from(anyhow!("保存配置失败: {}", e)))?;

    // 检查是否正在扫描，如果是则通过任务队列处理
    if crate::task::is_scanning() {
        // 将配置更新任务加入队列
        use uuid::Uuid;
        let reload_task = crate::task::ReloadConfigTask {
            task_id: Uuid::new_v4().to_string(),
        };
        crate::task::enqueue_reload_task(reload_task, &db).await?;
        info!("检测到正在扫描，凭证保存任务已加入队列");
    } else {
        // 只更新凭据配置项，避免覆盖其他配置
        use crate::config::ConfigManager;
        let manager = ConfigManager::new(db.as_ref().clone());

        let credential_json = serde_json::to_value(&config.credential).map_err(|e| {
            warn!("序列化凭据失败: {}", e);
            e
        });

        if let Ok(credential_value) = credential_json {
            if let Err(e) = manager.update_config_item("credential", credential_value).await {
                warn!("更新凭据配置失败: {}", e);
            } else {
                info!("凭证已保存到数据库");
            }
        }

        // 重新加载全局配置包（从数据库）
        if let Err(e) = crate::config::reload_config_bundle().await {
            warn!("重新加载配置包失败: {}", e);
            // 回退到传统的重新加载方式
            crate::config::reload_config();
        }
    }

    let response = crate::api::response::UpdateCredentialResponse {
        success: true,
        message: "B站凭证更新成功".to_string(),
    };

    Ok(ApiResponse::ok(response))
}

/// 生成扫码登录二维码
#[utoipa::path(
    post,
    path = "/api/auth/qr/generate",
    request_body = QRGenerateRequest,
    responses(
        (status = 200, description = "生成二维码成功", body = QRGenerateResponse),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn generate_qr_code(
    axum::Json(_params): axum::Json<crate::api::request::QRGenerateRequest>,
) -> Result<ApiResponse<crate::api::response::QRGenerateResponse>, ApiError> {
    info!("收到生成二维码请求");

    // 生成二维码
    let (session_id, qr_info) = match QR_SERVICE.generate_qr_code().await {
        Ok(result) => {
            info!("生成二维码成功: session_id={}", result.0);
            result
        }
        Err(e) => {
            error!("生成二维码失败: {}", e);
            return Err(ApiError::from(anyhow!("生成二维码失败: {}", e)));
        }
    };

    let response = crate::api::response::QRGenerateResponse {
        session_id,
        qr_url: qr_info.url,
        expires_in: 180, // 3分钟
    };

    Ok(ApiResponse::ok(response))
}

/// 轮询扫码登录状态
#[utoipa::path(
    get,
    path = "/api/auth/qr/poll",
    params(QRPollRequest),
    responses(
        (status = 200, description = "获取状态成功", body = QRPollResponse),
        (status = 400, description = "请求参数错误", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn poll_qr_status(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Query(params): Query<crate::api::request::QRPollRequest>,
) -> Result<ApiResponse<crate::api::response::QRPollResponse>, ApiError> {
    debug!("收到轮询请求: session_id={}", params.session_id);

    // 轮询登录状态
    let status = match QR_SERVICE.poll_login_status(&params.session_id).await {
        Ok(s) => {
            // 根据状态决定日志级别：Pending/Scanned 使用 debug，Confirmed 使用 info
            match &s {
                crate::auth::LoginStatus::Confirmed(_) => {
                    info!("轮询成功: session_id={}, status={:?}", params.session_id, s);
                }
                _ => {
                    debug!("轮询成功: session_id={}, status={:?}", params.session_id, s);
                }
            }
            s
        }
        Err(e) => {
            error!("轮询失败: session_id={}, error={}", params.session_id, e);
            return Err(ApiError::from(anyhow!("轮询状态失败: {}", e)));
        }
    };

    use crate::auth::LoginStatus;
    let response = match status {
        LoginStatus::Pending => crate::api::response::QRPollResponse {
            status: "pending".to_string(),
            message: "等待扫码".to_string(),
            user_info: None,
        },
        LoginStatus::Scanned => crate::api::response::QRPollResponse {
            status: "scanned".to_string(),
            message: "已扫码，请在手机上确认".to_string(),
            user_info: None,
        },
        LoginStatus::Confirmed(login_result) => {
            // 保存凭证到配置系统
            let config = crate::config::reload_config();
            config
                .credential
                .store(Some(std::sync::Arc::new(login_result.credential.clone())));

            // 检查是否正在扫描，如果是则通过任务队列处理
            if crate::task::is_scanning() {
                // 将配置更新任务加入队列
                use uuid::Uuid;
                let reload_task = crate::task::ReloadConfigTask {
                    task_id: Uuid::new_v4().to_string(),
                };
                crate::task::enqueue_reload_task(reload_task, &db)
                    .await
                    .map_err(|e| ApiError::from(anyhow!("保存凭证失败: {}", e)))?;
                info!("检测到正在扫描，凭证保存任务已加入队列");
            } else {
                // 只更新凭据配置项，避免覆盖其他配置
                use crate::config::ConfigManager;
                let manager = ConfigManager::new(db.as_ref().clone());

                let credential_json = serde_json::to_value(&config.credential).map_err(|e| {
                    error!("序列化凭据失败: {}", e);
                    ApiError::from(anyhow!("序列化凭据失败: {}", e))
                })?;

                if let Err(e) = manager.update_config_item("credential", credential_json).await {
                    error!("保存凭证到数据库失败: {}", e);
                    return Err(ApiError::from(anyhow!("保存凭证失败: {}", e)));
                } else {
                    info!("扫码登录凭证已保存到数据库");
                }

                // 重新加载全局配置包（从数据库）
                if let Err(e) = crate::config::reload_config_bundle().await {
                    warn!("重新加载配置包失败: {}", e);
                    // 回退到传统的重新加载方式
                    crate::config::reload_config();
                }

                // 用户登录成功后，尝试初始化硬件指纹
                use crate::hardware::HardwareFingerprint;
                if let Err(e) = HardwareFingerprint::reinit_if_user_changed(db.as_ref()).await {
                    debug!("硬件指纹初始化失败: {}", e);
                } else {
                    info!("登录后硬件指纹初始化完成");
                }
            }

            crate::api::response::QRPollResponse {
                status: "confirmed".to_string(),
                message: "登录成功".to_string(),
                user_info: Some(crate::api::response::QRUserInfo {
                    user_id: login_result.user_info.user_id,
                    username: login_result.user_info.username,
                    avatar_url: login_result.user_info.avatar_url,
                }),
            }
        }
        LoginStatus::Expired => crate::api::response::QRPollResponse {
            status: "expired".to_string(),
            message: "二维码已过期".to_string(),
            user_info: None,
        },
        LoginStatus::Error(msg) => crate::api::response::QRPollResponse {
            status: "error".to_string(),
            message: msg,
            user_info: None,
        },
    };

    Ok(ApiResponse::ok(response))
}

/// 获取当前用户信息
#[utoipa::path(
    get,
    path = "/api/auth/current-user",
    responses(
        (status = 200, description = "获取成功", body = QRUserInfo),
        (status = 401, description = "未登录或凭证无效"),
        (status = 500, description = "服务器内部错误")
    )
)]
pub async fn get_current_user() -> Result<ApiResponse<crate::api::response::QRUserInfo>, ApiError> {
    // 获取当前凭证
    let config = crate::config::with_config(|bundle| bundle.config.clone());
    let credential = config.credential.load();

    let cred = match credential.as_deref() {
        Some(cred) => cred,
        None => return Err(anyhow::anyhow!("未找到有效凭证").into()),
    };

    // 构建cookie字符串
    let cookie_str = format!(
        "SESSDATA={}; bili_jct={}; buvid3={}; DedeUserID={}",
        cred.sessdata, cred.bili_jct, cred.buvid3, cred.dedeuserid
    );

    // 创建 HTTP 客户端
    let client = reqwest::Client::new();

    // 调用B站API获取用户信息
    let request_url = "https://api.bilibili.com/x/web-interface/nav";
    tracing::debug!("发起用户信息请求: {} - User ID: {}", request_url, cred.dedeuserid);
    tracing::debug!(
        "用户信息请求将携带凭证（sessdata_len={}, bili_jct_len={}, buvid3_len={}, has_buvid4={}）",
        cred.sessdata.len(),
        cred.bili_jct.len(),
        cred.buvid3.len(),
        cred.buvid4.is_some()
    );

    let request = client
        .get(request_url)
        .headers(create_api_headers())
        .header("Cookie", cookie_str);

    // 用户信息请求头日志已在建造器时设置

    let response = request.send().await;
    let response = match response {
        Ok(resp) => {
            tracing::debug!("用户信息请求成功 - 状态码: {}, URL: {}", resp.status(), resp.url());
            resp
        }
        Err(e) => {
            tracing::error!("用户信息请求失败 - User ID: {}, 错误: {}", cred.dedeuserid, e);
            return Err(anyhow::anyhow!("请求B站API失败: {}", e).into());
        }
    };

    let data: serde_json::Value = match response.json().await {
        Ok(json) => {
            tracing::debug!("用户信息响应解析成功 - User ID: {}", cred.dedeuserid);
            json
        }
        Err(e) => {
            tracing::error!("用户信息响应解析失败 - User ID: {}, 错误: {}", cred.dedeuserid, e);
            return Err(anyhow::anyhow!("解析响应失败: {}", e).into());
        }
    };

    if data["code"].as_i64() != Some(0) {
        return Err(anyhow::anyhow!(
            "获取用户信息失败: {}",
            data["message"].as_str().unwrap_or("Unknown error")
        )
        .into());
    }

    let user_data = &data["data"];
    Ok(ApiResponse::ok(crate::api::response::QRUserInfo {
        user_id: user_data["mid"].as_i64().unwrap_or(0).to_string(),
        username: user_data["uname"].as_str().unwrap_or("").to_string(),
        avatar_url: user_data["face"].as_str().unwrap_or("").to_string(),
    }))
}

/// 清除当前凭证
#[utoipa::path(
    post,
    path = "/api/auth/clear-credential",
    responses(
        (status = 200, description = "清除成功", body = ApiResponse<UpdateCredentialResponse>),
        (status = 500, description = "服务器内部错误")
    )
)]
pub async fn clear_credential() -> Result<ApiResponse<UpdateCredentialResponse>, ApiError> {
    use crate::bilibili::Credential;

    // 清空凭证
    let empty_credential = Credential {
        sessdata: String::new(),
        bili_jct: String::new(),
        buvid3: String::new(),
        dedeuserid: String::new(),
        ac_time_value: String::new(),
        buvid4: None,
        dedeuserid_ckmd5: None,
    };

    // 获取配置管理器并保存空凭证
    let config_manager = crate::config::get_config_manager().ok_or_else(|| anyhow::anyhow!("配置管理器未初始化"))?;
    config_manager
        .update_config_item("credential", serde_json::to_value(&empty_credential)?)
        .await?;

    // 更新内存中的配置
    crate::config::with_config(|bundle| {
        bundle.config.credential.store(None);
    });

    Ok(ApiResponse::ok(UpdateCredentialResponse {
        success: true,
        message: "凭证已清除".to_string(),
    }))
}

/// 暂停扫描功能
#[utoipa::path(
    post,
    path = "/api/task-control/pause",
    responses(
        (status = 200, description = "暂停成功", body = crate::api::response::TaskControlResponse),
        (status = 500, description = "内部错误")
    )
)]
pub async fn pause_scanning_endpoint() -> Result<ApiResponse<crate::api::response::TaskControlResponse>, ApiError> {
    crate::task::pause_scanning().await;
    Ok(ApiResponse::ok(crate::api::response::TaskControlResponse {
        success: true,
        message: "已暂停所有扫描和下载任务".to_string(),
        is_paused: true,
    }))
}

/// 恢复扫描功能
#[utoipa::path(
    post,
    path = "/api/task-control/resume",
    responses(
        (status = 200, description = "恢复成功", body = crate::api::response::TaskControlResponse),
        (status = 500, description = "内部错误")
    )
)]
pub async fn resume_scanning_endpoint() -> Result<ApiResponse<crate::api::response::TaskControlResponse>, ApiError> {
    crate::task::resume_scanning();
    Ok(ApiResponse::ok(crate::api::response::TaskControlResponse {
        success: true,
        message: "已恢复所有扫描和下载任务".to_string(),
        is_paused: false,
    }))
}

/// 获取任务控制状态
#[utoipa::path(
    get,
    path = "/api/task-control/status",
    responses(
        (status = 200, description = "获取状态成功", body = crate::api::response::TaskControlStatusResponse),
        (status = 500, description = "内部错误")
    )
)]
pub async fn get_task_control_status() -> Result<ApiResponse<crate::api::response::TaskControlStatusResponse>, ApiError>
{
    let is_paused = crate::task::TASK_CONTROLLER.is_paused();
    let is_scanning = crate::task::TASK_CONTROLLER.is_scanning();

    Ok(ApiResponse::ok(crate::api::response::TaskControlStatusResponse {
        is_paused,
        is_scanning,
        message: if is_paused {
            "任务已暂停".to_string()
        } else if is_scanning {
            "正在扫描中".to_string()
        } else {
            "任务空闲".to_string()
        },
    }))
}

/// 立即刷新任务（触发立即扫描/下载，无需等待下一次定时触发）
#[utoipa::path(
    post,
    path = "/api/task-control/refresh",
    responses(
        (status = 200, description = "刷新成功", body = crate::api::response::TaskControlResponse),
        (status = 500, description = "内部错误")
    )
)]
pub async fn refresh_scanning_endpoint(
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<crate::api::response::TaskControlResponse>, ApiError> {
    // 任务刷新属于用户的“立即执行”操作，应绕过投稿源的自适应下一次扫描时间限制。
    // 这里仅清空 next_scan_at，不改 no_update_streak，避免破坏自适应统计。
    match submission::Entity::update_many()
        .col_expr(submission::Column::NextScanAt, Expr::value(Option::<String>::None))
        .filter(submission::Column::Enabled.eq(true))
        .exec(db.as_ref())
        .await
    {
        Ok(res) => {
            if res.rows_affected > 0 {
                info!(
                    "任务刷新：已清空 {} 个投稿源的 next_scan_at，立即允许扫描",
                    res.rows_affected
                );
            }
        }
        Err(e) => {
            // 不阻断刷新流程，避免用户侧“点了刷新却无响应”
            warn!("任务刷新时清空投稿源 next_scan_at 失败: {}", e);
        }
    }

    // 若暂停中，则先恢复；无论是否暂停，都触发一次立即扫描
    if crate::task::TASK_CONTROLLER.is_paused() {
        crate::task::resume_scanning();
    } else {
        crate::task::TASK_CONTROLLER.trigger_scan_now();
    }

    Ok(ApiResponse::ok(crate::api::response::TaskControlResponse {
        success: true,
        message: "已触发任务刷新，将立即开始新一轮扫描".to_string(),
        is_paused: false,
    }))
}

#[derive(Deserialize, utoipa::ToSchema, Default)]
pub struct LatestIngestQuery {
    /// 返回条数，默认 10，最大 100
    pub limit: Option<usize>,
}

/// 获取首页「最新入库」列表
#[utoipa::path(
    get,
    path = "/api/ingest/latest",
    params(
        ("limit" = Option<usize>, Query, description = "返回条数，默认 10，最大 100")
    ),
    responses(
        (status = 200, description = "获取成功", body = crate::api::response::LatestIngestResponse),
        (status = 500, description = "内部错误")
    )
)]
pub async fn get_latest_ingests(
    Query(query): Query<LatestIngestQuery>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<crate::api::response::LatestIngestResponse>, ApiError> {
    let limit = query.limit.unwrap_or(10).clamp(1, 100);

    // 1) 先取内存事件（带速度）
    let mut items = crate::ingest_log::INGEST_LOG.list_latest(limit).await;

    // 2) 不足时再用 DB 补齐（速度可能为空，但保证首页总能显示）
    if items.len() < limit {
        let need = limit - items.len();
        let mut existing_ids = std::collections::HashSet::new();
        for it in &items {
            existing_ids.insert(it.video_id);
        }

        // 只查询已完成的视频（download_status >= STATUS_COMPLETED，即最高位为1）
        let fallback = video::Entity::find()
            .filter(video::Column::DownloadStatus.gte(crate::utils::status::STATUS_COMPLETED))
            .order_by_desc(video::Column::CreatedAt)
            .limit(need as u64)
            .all(db.as_ref())
            .await
            .map_err(|e| ApiError::from(InnerApiError::from(e)))?;

        for v in fallback {
            if existing_ids.contains(&v.id) {
                continue;
            }
            // 通过 deleted 字段和 status bits 判断状态
            use crate::ingest_log::IngestStatus;
            let status = if v.deleted != 0 {
                IngestStatus::Deleted
            } else {
                let st = VideoStatus::from(v.download_status);
                let bits: [u32; 5] = st.into();
                if bits.iter().all(|&b| b == crate::utils::status::STATUS_OK) {
                    IngestStatus::Success
                } else {
                    IngestStatus::Failed
                }
            };

            // 从 share_copy 提取番剧系列名称（《剧名》格式）
            let series_name = v.share_copy.as_ref().and_then(|s| {
                if let Some(start) = s.find('《') {
                    if let Some(end) = s.find('》') {
                        if end > start {
                            return Some(s[start + 3..end].to_string()); // UTF-8 《 is 3 bytes
                        }
                    }
                }
                None
            });

            items.push(crate::ingest_log::IngestEvent {
                video_id: v.id,
                video_name: v.name.clone(),
                upper_name: v.upper_name.clone(),
                path: v.path.clone(),
                ingested_at: v.created_at.clone(),
                download_speed_bps: None,
                status,
                series_name,
            });
        }
    }

    // 3) 转响应结构
    let resp_items = items
        .into_iter()
        .map(|e| {
            use crate::ingest_log::IngestStatus;
            let status_str = match e.status {
                IngestStatus::Success => "success",
                IngestStatus::Failed => "failed",
                IngestStatus::Deleted => "deleted",
            };
            crate::api::response::LatestIngestItemResponse {
                video_id: e.video_id,
                video_name: e.video_name,
                upper_name: e.upper_name,
                path: e.path,
                ingested_at: e.ingested_at,
                download_speed_bps: e.download_speed_bps,
                status: status_str.to_string(),
                series_name: e.series_name,
            }
        })
        .collect();

    Ok(ApiResponse::ok(crate::api::response::LatestIngestResponse {
        items: resp_items,
    }))
}

/// 获取视频的BVID信息（用于构建B站链接）
#[utoipa::path(
    get,
    path = "/api/videos/{video_id}/bvid",
    params(
        ("video_id" = String, Path, description = "视频ID或分页ID")
    ),
    responses(
        (status = 200, description = "获取BVID成功", body = crate::api::response::VideoBvidResponse),
        (status = 404, description = "视频不存在"),
        (status = 500, description = "内部错误")
    )
)]
pub async fn get_video_bvid(
    Path(video_id): Path<String>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<crate::api::response::VideoBvidResponse>, ApiError> {
    use crate::api::response::VideoBvidResponse;

    // 查找视频信息
    let video_info = find_video_info(&video_id, &db)
        .await
        .map_err(|e| ApiError::from(anyhow!("获取视频信息失败: {}", e)))?;

    Ok(ApiResponse::ok(VideoBvidResponse {
        bvid: video_info.bvid.clone(),
        title: video_info.title.clone(),
        bilibili_url:
            // 根据视频类型生成正确的B站URL
            if video_info.source_type == Some(1) && video_info.ep_id.is_some() {
                // 番剧类型：使用 ep_id 生成番剧专用URL
                format!("https://www.bilibili.com/bangumi/play/ep{}", video_info.ep_id.as_ref().unwrap())
            } else {
                // 普通视频：使用 bvid 生成视频URL
                format!("https://www.bilibili.com/video/{}", video_info.bvid)
            },
    }))
}

#[derive(Debug, Serialize, Deserialize)]
struct VideoPlayStreamCache {
    video_streams: Vec<crate::api::response::VideoStreamInfo>,
    audio_streams: Vec<crate::api::response::AudioStreamInfo>,
    subtitle_streams: Vec<crate::api::response::SubtitleStreamInfo>,
    updated_at: Option<String>,
}

fn summarize_stream_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("-");
        let path = parsed.path();
        let query_len = parsed.query().map(|q| q.len()).unwrap_or(0);
        return format!("{}://{}{} (query_len={})", parsed.scheme(), host, path, query_len);
    }
    format!("invalid_or_relative_url(len={})", url.len())
}

async fn load_video_play_stream_cache(
    db: &DatabaseConnection,
    page_id: i32,
) -> Result<Option<VideoPlayStreamCache>, anyhow::Error> {
    let Some(page_model) = page::Entity::find_by_id(page_id)
        .one(db)
        .await
        .context("查询分页缓存失败")?
    else {
        debug!("在线播放缓存未命中: page_id={}, reason=page_not_found", page_id);
        return Ok(None);
    };

    let Some(video_streams_raw) = page_model.play_video_streams.as_ref() else {
        debug!(
            "在线播放缓存未命中: page_id={}, reason=play_video_streams_empty",
            page_id
        );
        return Ok(None);
    };

    let video_streams: Vec<crate::api::response::VideoStreamInfo> =
        serde_json::from_str(video_streams_raw).context("解析缓存视频流失败")?;
    if video_streams.is_empty() {
        debug!(
            "在线播放缓存未命中: page_id={}, reason=video_streams_empty_after_parse",
            page_id
        );
        return Ok(None);
    }

    let audio_streams: Vec<crate::api::response::AudioStreamInfo> = match page_model.play_audio_streams.as_ref() {
        Some(raw) => match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                debug!(
                    "在线播放缓存音频流解析失败，已忽略: page_id={}, raw_len={}, error={}",
                    page_id,
                    raw.len(),
                    e
                );
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    let subtitle_streams: Vec<crate::api::response::SubtitleStreamInfo> =
        match page_model.play_subtitle_streams.as_ref() {
            Some(raw) => match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(e) => {
                    debug!(
                        "在线播放缓存字幕流解析失败，已忽略: page_id={}, raw_len={}, error={}",
                        page_id,
                        raw.len(),
                        e
                    );
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

    let first_video_url = video_streams
        .first()
        .map(|stream| summarize_stream_url(&stream.url))
        .unwrap_or_else(|| "-".to_string());
    debug!(
        "在线播放缓存加载成功: page_id={}, updated_at={:?}, video_streams={}, audio_streams={}, subtitle_streams={}, first_video_url={}",
        page_id,
        page_model.play_streams_updated_at,
        video_streams.len(),
        audio_streams.len(),
        subtitle_streams.len(),
        first_video_url
    );

    Ok(Some(VideoPlayStreamCache {
        video_streams,
        audio_streams,
        subtitle_streams,
        updated_at: page_model.play_streams_updated_at,
    }))
}

async fn save_video_play_stream_cache(
    db: &DatabaseConnection,
    page_id: i32,
    cache: &VideoPlayStreamCache,
) -> Result<(), anyhow::Error> {
    let updated_at = now_standard_string();
    let video_streams_raw = serde_json::to_string(&cache.video_streams).context("序列化视频流缓存失败")?;
    let audio_streams_raw = serde_json::to_string(&cache.audio_streams).context("序列化音频流缓存失败")?;
    let subtitle_streams_raw = serde_json::to_string(&cache.subtitle_streams).context("序列化字幕缓存失败")?;
    let first_video_url = cache
        .video_streams
        .first()
        .map(|stream| summarize_stream_url(&stream.url))
        .unwrap_or_else(|| "-".to_string());
    debug!(
        "写入在线播放缓存: page_id={}, updated_at={}, video_streams={}, audio_streams={}, subtitle_streams={}, payload_len(video/audio/subtitle)={}/{}/{}, first_video_url={}",
        page_id,
        updated_at,
        cache.video_streams.len(),
        cache.audio_streams.len(),
        cache.subtitle_streams.len(),
        video_streams_raw.len(),
        audio_streams_raw.len(),
        subtitle_streams_raw.len(),
        first_video_url
    );

    let update_model = page::ActiveModel {
        id: Unchanged(page_id),
        play_video_streams: Set(Some(video_streams_raw)),
        play_audio_streams: Set(Some(audio_streams_raw)),
        play_subtitle_streams: Set(Some(subtitle_streams_raw)),
        play_streams_updated_at: Set(Some(updated_at)),
        ..Default::default()
    };

    update_model.update(db).await.context("写入播放缓存失败")?;
    debug!("写入在线播放缓存成功: page_id={}", page_id);
    Ok(())
}

/// 获取视频播放信息（在线播放用）
#[derive(Debug, Deserialize)]
pub struct VideoPlayInfoQuery {
    #[serde(default)]
    pub refresh: bool,
}

#[utoipa::path(
    get,
    path = "/api/videos/{video_id}/play-info",
    params(
        ("video_id" = String, Path, description = "视频ID或分页ID"),
        ("refresh" = Option<bool>, Query, description = "是否强制刷新播放地址缓存")
    ),
    responses(
        (status = 200, description = "获取播放信息成功", body = crate::api::response::VideoPlayInfoResponse),
        (status = 404, description = "视频不存在"),
        (status = 500, description = "内部错误")
    )
)]
pub async fn get_video_play_info(
    Path(video_id): Path<String>,
    Query(play_query): Query<VideoPlayInfoQuery>,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<crate::api::response::VideoPlayInfoResponse>, ApiError> {
    use crate::api::response::{AudioStreamInfo, SubtitleStreamInfo, VideoPlayInfoResponse, VideoStreamInfo};
    use crate::bilibili::{BestStream, BiliClient, PageInfo, Stream, Video};

    let force_refresh = play_query.refresh;
    debug!("收到在线播放请求: video_id={}, refresh={}", video_id, force_refresh);

    // 查找视频信息
    let video_info = find_video_info(&video_id, &db)
        .await
        .map_err(|e| ApiError::from(anyhow!("获取视频信息失败: {}", e)))?;

    debug!(
        "在线播放视频信息解析成功: page_id={}, bvid={}, aid={}, cid={}, duration={}, source_type={:?}, ep_id={:?}",
        video_info.page_id,
        video_info.bvid,
        video_info.aid,
        video_info.cid,
        video_info.duration,
        video_info.source_type,
        video_info.ep_id
    );

    // 获取分页信息
    let page_info = PageInfo {
        cid: video_info
            .cid
            .parse()
            .map_err(|_| ApiError::from(anyhow!("无效的CID")))?,
        page: 1,
        name: video_info.title.clone(),
        duration: video_info.duration,
        first_frame: None,
        dimension: None,
    };

    let video_title = video_info.title.clone();
    let bilibili_url =
        // 根据视频类型生成正确的B站URL
        if video_info.source_type == Some(1) && video_info.ep_id.is_some() {
            // 番剧类型：使用 ep_id 生成番剧专用URL
            format!(
                "https://www.bilibili.com/bangumi/play/ep{}",
                video_info.ep_id.as_ref().unwrap()
            )
        } else {
            // 普通视频：使用 bvid 生成视频URL
            format!("https://www.bilibili.com/video/{}", video_info.bvid)
        };

    let build_error_message = |err: &anyhow::Error| -> String {
        // 先给常见“未登录/无凭证”一个更直观的提示
        let err_str = err.to_string();
        if err_str.contains("no credential found") || err_str.contains("未设置") {
            return "未设置或未完整设置 B 站登录凭证，无法获取在线播放信息".to_string();
        }

        if let Some(bili_err) = err.downcast_ref::<crate::bilibili::BiliError>() {
            match bili_err {
                crate::bilibili::BiliError::RiskControlVerificationRequired(_) => {
                    "触发B站风控，需要在管理页“验证码”完成验证后重试".to_string()
                }
                crate::bilibili::BiliError::RiskControlOccurred => {
                    "触发B站风控，请稍后重试（频繁出现可尝试刷新凭证或完成验证码验证）".to_string()
                }
                crate::bilibili::BiliError::RequestFailed(-404, _) => "视频已被删除或不存在".to_string(),
                crate::bilibili::BiliError::VideoStreamEmpty(_) => "没有可用的视频流".to_string(),
                _ => bili_err.to_string(),
            }
        } else {
            err_str
        }
    };

    let fail = |message: String| {
        ApiResponse::ok(VideoPlayInfoResponse {
            success: false,
            message: Some(message),
            video_streams: Vec::new(),
            audio_streams: Vec::new(),
            subtitle_streams: Vec::new(),
            video_title: video_title.clone(),
            video_duration: Some(page_info.duration),
            video_quality_description: "获取失败".to_string(),
            video_bvid: Some(video_info.bvid.clone()),
            bilibili_url: Some(bilibili_url.clone()),
        })
    };
    let build_success_response = |video_streams: Vec<VideoStreamInfo>,
                                  audio_streams: Vec<AudioStreamInfo>,
                                  subtitle_streams: Vec<SubtitleStreamInfo>| {
        let quality_desc = if !video_streams.is_empty() {
            video_streams[0].quality_description.clone()
        } else {
            "未知".to_string()
        };

        VideoPlayInfoResponse {
            success: true,
            message: None,
            video_streams,
            audio_streams,
            subtitle_streams,
            video_title: video_title.clone(),
            video_duration: Some(page_info.duration),
            video_quality_description: quality_desc,
            video_bvid: Some(video_info.bvid.clone()),
            bilibili_url: Some(bilibili_url.clone()),
        }
    };

    if !force_refresh {
        match load_video_play_stream_cache(&db, video_info.page_id).await {
            Ok(Some(cache)) => {
                let first_video_url = cache
                    .video_streams
                    .first()
                    .map(|stream| summarize_stream_url(&stream.url))
                    .unwrap_or_else(|| "-".to_string());
                debug!(
                    "在线播放命中缓存: page_id={}, updated_at={:?}, video_streams={}, audio_streams={}, subtitle_streams={}, first_video_url={}",
                    video_info.page_id,
                    cache.updated_at,
                    cache.video_streams.len(),
                    cache.audio_streams.len(),
                    cache.subtitle_streams.len(),
                    first_video_url
                );
                return Ok(ApiResponse::ok(build_success_response(
                    cache.video_streams,
                    cache.audio_streams,
                    cache.subtitle_streams,
                )));
            }
            Ok(None) => {
                debug!("在线播放缓存未命中: page_id={}", video_info.page_id);
            }
            Err(e) => {
                warn!(
                    "读取在线播放缓存失败，改为实时获取: page_id={}, error={}",
                    video_info.page_id, e
                );
            }
        }
    } else {
        debug!("在线播放强制刷新缓存: page_id={}", video_info.page_id);
    }

    // 创建B站客户端（仅在缓存未命中或强制刷新时执行）
    let config = crate::config::reload_config();
    let credential = config.credential.load();
    let cookie_string = credential
        .as_ref()
        .map(|cred| {
            format!(
                "SESSDATA={};bili_jct={};buvid3={};DedeUserID={};ac_time_value={}",
                cred.sessdata, cred.bili_jct, cred.buvid3, cred.dedeuserid, cred.ac_time_value
            )
        })
        .unwrap_or_default();
    let bili_client = BiliClient::new(cookie_string);

    // 创建Video实例
    let video = Video::new_with_aid(&bili_client, video_info.bvid.clone(), video_info.aid.clone());

    // 使用用户配置的筛选选项（用于控制请求的画质范围，避免 qn=127 导致只返回高画质从而被本地过滤掉）
    let filter_option = config.filter_option.clone();
    let max_qn = filter_option.video_max_quality as u32;
    let min_qn = filter_option.video_min_quality as u32;

    // 获取视频播放链接 - 根据视频类型选择不同的API
    let mut page_analyzer = if video_info.source_type == Some(1) && video_info.ep_id.is_some() {
        // 使用番剧专用API
        let ep_id = video_info.ep_id.as_ref().unwrap();
        debug!("API播放使用番剧专用API: ep_id={}", ep_id);
        match video
            .get_bangumi_page_analyzer_with_fallback_in_range(&page_info, ep_id, max_qn, min_qn)
            .await
        {
            Ok(analyzer) => analyzer,
            Err(e) => {
                warn!("获取番剧视频分析器失败: {:#}", e);
                return Ok(fail(build_error_message(&e)));
            }
        }
    } else {
        // 使用普通视频API
        match video
            .get_page_analyzer_with_fallback_in_range(&page_info, max_qn, min_qn)
            .await
        {
            Ok(analyzer) => analyzer,
            Err(e) => {
                warn!("获取视频分析器失败: {:#}", e);
                return Ok(fail(build_error_message(&e)));
            }
        }
    };

    let best_stream = match page_analyzer.best_stream(&filter_option) {
        Ok(stream) => stream,
        Err(e) => {
            warn!("获取最佳视频流失败: {:#}", e);
            return Ok(fail(format!("获取最佳视频流失败: {}", e)));
        }
    };

    debug!(
        "获取到的流类型: {:?}",
        match &best_stream {
            BestStream::VideoAudio { .. } => "DASH视频+音频分离流",
            BestStream::Mixed(_) => "混合流（包含音频）",
        }
    );

    let mut video_streams = Vec::new();
    let mut audio_streams = Vec::new();

    match best_stream {
        BestStream::VideoAudio {
            video: video_stream,
            audio: audio_stream,
        } => {
            // 使用与下载流程相同的方式获取URL
            let video_urls = video_stream.urls();

            // 处理视频流 - 使用第一个可用URL作为主URL，其余作为备用
            if let Some((main_url, backup_urls)) = video_urls.split_first() {
                if let Stream::DashVideo { quality, codecs, .. } = &video_stream {
                    video_streams.push(VideoStreamInfo {
                        url: main_url.to_string(),
                        backup_urls: backup_urls.iter().map(|s| s.to_string()).collect(),
                        quality: *quality as u32,
                        quality_description: get_video_quality_description(*quality),
                        codecs: get_video_codecs_description(*codecs),
                        container: Some("dash".to_string()),
                        width: None,
                        height: None,
                    });
                }
            }

            // 处理音频流
            if let Some(audio_stream) = audio_stream {
                let audio_urls = audio_stream.urls();
                if let Some((main_url, backup_urls)) = audio_urls.split_first() {
                    if let Stream::DashAudio { quality, .. } = &audio_stream {
                        audio_streams.push(AudioStreamInfo {
                            url: main_url.to_string(),
                            backup_urls: backup_urls.iter().map(|s| s.to_string()).collect(),
                            quality: *quality as u32,
                            quality_description: get_audio_quality_description(*quality),
                        });
                    }
                }
            }
        }
        BestStream::Mixed(stream) => {
            // 处理混合流（FLV或MP4）- 使用与下载流程相同的方式
            let urls = stream.urls();
            if let Some((main_url, backup_urls)) = urls.split_first() {
                let container = match stream {
                    Stream::Flv(_) => Some("flv".to_string()),
                    Stream::Html5Mp4(_) | Stream::EpisodeTryMp4(_) => Some("mp4".to_string()),
                    _ => None,
                };
                video_streams.push(VideoStreamInfo {
                    url: main_url.to_string(),
                    backup_urls: backup_urls.iter().map(|s| s.to_string()).collect(),
                    quality: 0, // 混合流没有具体质量信息
                    quality_description: "混合流".to_string(),
                    codecs: "未知".to_string(),
                    container,
                    width: None,
                    height: None,
                });
            }
        }
    }

    // 获取字幕信息
    let subtitle_streams = match video.get_subtitles(&page_info).await {
        Ok(subtitles) => {
            subtitles
                .into_iter()
                .map(|subtitle| SubtitleStreamInfo {
                    language: subtitle.lan.clone(),
                    language_doc: subtitle.lan.clone(), // 暂时使用language作为language_doc
                    url: format!("/api/videos/{}/subtitles/{}", video_id, subtitle.lan),
                })
                .collect()
        }
        Err(e) => {
            warn!("获取字幕失败: {}", e);
            Vec::new()
        }
    };

    let cache_payload = VideoPlayStreamCache {
        video_streams: video_streams.clone(),
        audio_streams: audio_streams.clone(),
        subtitle_streams: subtitle_streams.clone(),
        updated_at: None,
    };
    if let Err(e) = save_video_play_stream_cache(&db, video_info.page_id, &cache_payload).await {
        warn!("写入在线播放缓存失败: page_id={}, error={}", video_info.page_id, e);
    }
    let first_video_url = video_streams
        .first()
        .map(|stream| summarize_stream_url(&stream.url))
        .unwrap_or_else(|| "-".to_string());
    let first_audio_url = audio_streams
        .first()
        .map(|stream| summarize_stream_url(&stream.url))
        .unwrap_or_else(|| "-".to_string());
    debug!(
        "在线播放回源成功: page_id={}, video_streams={}, audio_streams={}, subtitle_streams={}, first_video_url={}, first_audio_url={}",
        video_info.page_id,
        video_streams.len(),
        audio_streams.len(),
        subtitle_streams.len(),
        first_video_url,
        first_audio_url
    );

    Ok(ApiResponse::ok(build_success_response(
        video_streams,
        audio_streams,
        subtitle_streams,
    )))
}

/// 查找视频信息
#[derive(Debug)]
struct VideoPlayInfo {
    page_id: i32,
    bvid: String,
    aid: String,
    cid: String,
    duration: u32,
    title: String,
    source_type: Option<i32>,
    ep_id: Option<String>,
}

async fn find_video_info(video_id: &str, db: &DatabaseConnection) -> Result<VideoPlayInfo> {
    use crate::bilibili::bvid_to_aid;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    // 首先尝试作为分页ID查找
    if let Ok(page_id) = video_id.parse::<i32>() {
        if let Some(page_record) = page::Entity::find_by_id(page_id)
            .one(db)
            .await
            .context("查询分页记录失败")?
        {
            // 通过分页查找对应的视频
            if let Some(video_record) = video::Entity::find_by_id(page_record.video_id)
                .one(db)
                .await
                .context("查询视频记录失败")?
            {
                return Ok(VideoPlayInfo {
                    page_id: page_record.id,
                    bvid: video_record.bvid.clone(),
                    aid: bvid_to_aid(&video_record.bvid).to_string(),
                    cid: page_record.cid.to_string(),
                    duration: page_record.duration,
                    title: format!("{} - {}", video_record.name, page_record.name),
                    source_type: video_record.source_type,
                    ep_id: video_record.ep_id,
                });
            }
        }
    }

    // 尝试解析为视频ID
    let video_model = if let Ok(id) = video_id.parse::<i32>() {
        video::Entity::find_by_id(id)
            .one(db)
            .await
            .context("查询视频记录失败")?
    } else {
        // 按BVID查找
        video::Entity::find()
            .filter(video::Column::Bvid.eq(video_id))
            .one(db)
            .await
            .context("查询视频记录失败")?
    };

    let video = video_model.ok_or_else(|| anyhow::anyhow!("视频记录不存在: {}", video_id))?;

    // 获取第一个分页的cid
    let first_page = page::Entity::find()
        .filter(page::Column::VideoId.eq(video.id))
        .one(db)
        .await
        .context("查询视频分页失败")?
        .ok_or_else(|| anyhow::anyhow!("视频没有分页信息"))?;

    Ok(VideoPlayInfo {
        page_id: first_page.id,
        bvid: video.bvid.clone(),
        aid: bvid_to_aid(&video.bvid).to_string(),
        cid: first_page.cid.to_string(),
        duration: first_page.duration,
        title: video.name,
        source_type: video.source_type,
        ep_id: video.ep_id,
    })
}

/// 获取视频质量描述
fn get_video_quality_description(quality: crate::bilibili::VideoQuality) -> String {
    use crate::bilibili::VideoQuality;
    match quality {
        VideoQuality::Quality360p => "360P".to_string(),
        VideoQuality::Quality480p => "480P".to_string(),
        VideoQuality::Quality720p => "720P".to_string(),
        VideoQuality::Quality1080p => "1080P".to_string(),
        VideoQuality::Quality1080pPLUS => "1080P+".to_string(),
        VideoQuality::Quality1080p60 => "1080P60".to_string(),
        VideoQuality::Quality4k => "4K".to_string(),
        VideoQuality::QualityHdr => "HDR".to_string(),
        VideoQuality::QualityDolby => "杜比视界".to_string(),
        VideoQuality::Quality8k => "8K".to_string(),
    }
}

/// 获取音频质量描述
fn get_audio_quality_description(quality: crate::bilibili::AudioQuality) -> String {
    use crate::bilibili::AudioQuality;
    match quality {
        AudioQuality::Quality64k => "64K".to_string(),
        AudioQuality::Quality132k => "132K".to_string(),
        AudioQuality::Quality192k => "192K".to_string(),
        AudioQuality::QualityDolby | AudioQuality::QualityDolbyBangumi => "杜比全景声".to_string(),
        AudioQuality::QualityHiRES => "Hi-Res无损".to_string(),
    }
}

/// 获取视频编码描述
fn get_video_codecs_description(codecs: crate::bilibili::VideoCodecs) -> String {
    use crate::bilibili::VideoCodecs;
    match codecs {
        VideoCodecs::AVC => "AVC/H.264".to_string(),
        VideoCodecs::HEV => "HEVC/H.265".to_string(),
        VideoCodecs::AV1 => "AV1".to_string(),
    }
}

/// 代理B站视频流（解决跨域和防盗链）
#[utoipa::path(
    get,
    path = "/api/videos/proxy-stream",
    params(
        ("url" = String, Query, description = "要代理的视频流URL"),
        ("referer" = Option<String>, Query, description = "可选的Referer头")
    ),
    responses(
        (status = 200, description = "视频流代理成功"),
        (status = 400, description = "参数错误"),
        (status = 500, description = "代理失败")
    )
)]
pub async fn proxy_video_stream(
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    use axum::http::{header, HeaderValue, StatusCode};
    use axum::response::{IntoResponse, Response};
    use futures::StreamExt;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use tokio_util::io::ReaderStream;

    let stream_url = match params.get("url") {
        Some(url) => url,
        None => {
            return (StatusCode::BAD_REQUEST, "缺少url参数").into_response();
        }
    };

    // 检查认证信息
    let config = crate::config::reload_config();
    let credential = config.credential.load();
    debug!("当前认证信息是否存在: {}", credential.is_some());
    if let Some(cred) = credential.as_ref() {
        debug!(
            "认证信息已加载: DedeUserID={}, sessdata_len={}, bili_jct_len={}, buvid3_len={}, has_buvid4={}",
            cred.dedeuserid,
            cred.sessdata.len(),
            cred.bili_jct.len(),
            cred.buvid3.len(),
            cred.buvid4.is_some()
        );
    }

    // 使用与下载器相同的方式：只需要正确的默认头，不需要cookie认证
    debug!("使用与下载器相同的方式访问视频流，不添加cookie认证");

    // 检查Range请求
    let range_header = headers.get(header::RANGE).and_then(|h| h.to_str().ok());

    fn looks_like_flv_url(url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        lower.contains(".flv") || lower.contains("format=flv")
    }

    fn parse_bool_query(params: &std::collections::HashMap<String, String>, key: &str) -> bool {
        params.get(key).is_some_and(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "enable" | "enabled"
            )
        })
    }

    async fn transmux_flv_to_mp4(stream_url: &str) -> Result<Response, anyhow::Error> {
        use axum::http::{header, HeaderValue};

        let bili_client = crate::bilibili::BiliClient::new(String::new());
        let response = bili_client
            .request(reqwest::Method::GET, stream_url)
            .await
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!("B站视频流返回401未授权");
        }
        if !status.is_success() {
            anyhow::bail!("B站视频流返回错误状态: {}", status);
        }

        let mut cmd = tokio::process::Command::new(crate::downloader::resolve_media_tool_path("ffmpeg"));
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-c",
            "copy",
            "-f",
            "mp4",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        let mut child = cmd.spawn()?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg stdin 不可用"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg stdout 不可用"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg stderr 不可用"))?;

        let mut input_stream = response.bytes_stream();
        let input_task = tokio::spawn(async move {
            while let Some(chunk) = input_stream.next().await {
                let chunk = chunk?;
                stdin.write_all(&chunk).await?;
            }
            stdin.shutdown().await?;
            Ok::<(), anyhow::Error>(())
        });

        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;

            let mut reader = tokio::io::BufReader::new(stderr);
            let mut buf = Vec::new();
            let _ = reader.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).trim().to_string()
        });

        tokio::spawn(async move {
            let input_res = input_task.await;
            let status_res = child.wait().await;
            let stderr_text = stderr_task.await.unwrap_or_default();

            if let Err(e) = input_res {
                tracing::warn!("ffmpeg 输入流写入任务异常: {:#}", e);
            } else if let Ok(Err(e)) = input_res {
                tracing::warn!("ffmpeg 输入流写入失败: {:#}", e);
            }

            match status_res {
                Ok(exit_status) if exit_status.success() => {}
                Ok(exit_status) => {
                    tracing::warn!("ffmpeg 转封装失败: status={}, stderr={}", exit_status, stderr_text);
                }
                Err(e) => {
                    tracing::warn!("ffmpeg 进程等待失败: {:#}", e);
                }
            }
        });

        let body_stream = ReaderStream::new(stdout);
        let mut proxy_response = Response::new(axum::body::Body::from_stream(body_stream));

        // FLV 转封装后的输出不支持 Range，统一用 200（浏览器会自动处理）
        *proxy_response.status_mut() = axum::http::StatusCode::OK;

        let proxy_headers = proxy_response.headers_mut();
        proxy_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));

        // 添加CORS头
        proxy_headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        proxy_headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, HEAD, OPTIONS"),
        );
        proxy_headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Range"));

        // 设置缓存控制
        proxy_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));

        Ok(proxy_response)
    }

    // B站网页端老视频常用 flv.js 通过 Range 拉流实现可拖动播放。
    // 服务端默认直接代理原始流，前端若识别为 FLV 将使用 flv.js 播放，从而支持拖动。
    // 如需强制在服务端转封装为 mp4（用于不支持 flv.js/MSE 的环境），可传参 transmux=1。
    let transmux_enabled = parse_bool_query(&params, "transmux");
    debug!(
        "代理视频流请求: url={}, raw_url_len={}, range={:?}, transmux={}, is_flv={}",
        summarize_stream_url(stream_url),
        stream_url.len(),
        range_header,
        transmux_enabled,
        looks_like_flv_url(stream_url)
    );
    if transmux_enabled && looks_like_flv_url(stream_url) {
        match transmux_flv_to_mp4(stream_url).await {
            Ok(resp) => return resp,
            Err(e) => {
                error!("FLV 转封装代理失败: {:#}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "FLV 转封装失败，请检查 ffmpeg 是否可用",
                )
                    .into_response();
            }
        }
    }

    // 使用与下载器相同的Client设置进行流式代理
    let bili_client = crate::bilibili::BiliClient::new(String::new());
    let mut request_builder = bili_client
        .request(reqwest::Method::GET, stream_url)
        .await
        .header(header::ACCEPT_ENCODING, "identity");

    // 如果有Range请求，转发它
    if let Some(range) = range_header {
        request_builder = request_builder.header(header::RANGE, range);
    }

    // 发送请求
    let response = match request_builder.send().await {
        Ok(resp) => resp,
        Err(e) => {
            error!("代理请求失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "代理请求失败").into_response();
        }
    };

    let status = response.status();
    let response_headers = response.headers().clone();

    let content_type = response_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let content_length = response_headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let content_range = response_headers
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    let accept_ranges = response_headers
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    debug!(
        "代理视频流上游响应: url={}, status={}, content_type={}, content_length={}, content_range={}, accept_ranges={}",
        summarize_stream_url(stream_url),
        status,
        content_type,
        content_length,
        content_range,
        accept_ranges
    );

    // 如果是401错误，记录更多详细信息
    if status == reqwest::StatusCode::UNAUTHORIZED {
        error!("B站视频流返回401未授权错误");
        error!("请求URL: {}", stream_url);
        error!("使用下载器模式: 无cookie认证");
        return (StatusCode::UNAUTHORIZED, "B站视频流未授权").into_response();
    }

    // 如果是其他错误，也记录
    if !status.is_success() {
        error!("B站视频流返回错误状态: {}", status);
        return (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "B站视频流请求失败",
        )
            .into_response();
    }

    // 获取响应体
    // 获取响应流而不是一次性读取所有字节
    let body_stream = response.bytes_stream();

    // 构建流式响应
    let mut proxy_response = Response::new(axum::body::Body::from_stream(body_stream));
    *proxy_response.status_mut() = status;

    let proxy_headers = proxy_response.headers_mut();

    // 复制重要的响应头
    for (key, value) in response_headers.iter() {
        match key.as_str() {
            "content-type" | "content-length" | "content-range" | "accept-ranges" => {
                proxy_headers.insert(key, value.clone());
            }
            _ => {}
        }
    }

    // 添加CORS头
    proxy_headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    proxy_headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    proxy_headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Range"));
    proxy_headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Type, Content-Length, Content-Range, Accept-Ranges"),
    );

    // 设置缓存控制
    proxy_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));

    debug!(
        "代理视频流返回成功: url={}, status={}, range_forwarded={}",
        summarize_stream_url(stream_url),
        status,
        range_header.is_some()
    );
    proxy_response
}

/// 四步法安全重命名目录，避免父子目录冲突
/// 生成唯一的文件夹名称，避免同名冲突
fn generate_unique_folder_name(parent_dir: &std::path::Path, base_name: &str, bvid: &str, pubtime: &str) -> String {
    let mut unique_name = base_name.to_string();
    let mut counter = 0;

    // 检查基础名称是否已存在
    let base_path = parent_dir.join(&unique_name);
    if !base_path.exists() {
        return unique_name;
    }

    // 如果存在，先尝试追加发布时间
    unique_name = format!("{}-{}", base_name, pubtime);
    let time_path = parent_dir.join(&unique_name);
    if !time_path.exists() {
        info!("检测到文件夹名冲突，追加发布时间: {} -> {}", base_name, unique_name);
        return unique_name;
    }

    // 如果发布时间也冲突，追加BVID
    unique_name = format!("{}-{}", base_name, bvid);
    let bvid_path = parent_dir.join(&unique_name);
    if !bvid_path.exists() {
        info!("检测到文件夹名冲突，追加BVID: {} -> {}", base_name, unique_name);
        return unique_name;
    }

    // 如果都冲突，使用数字后缀
    loop {
        counter += 1;
        unique_name = format!("{}-{}", base_name, counter);
        let numbered_path = parent_dir.join(&unique_name);
        if !numbered_path.exists() {
            warn!("检测到严重文件夹名冲突，使用数字后缀: {} -> {}", base_name, unique_name);
            return unique_name;
        }

        // 防止无限循环
        if counter > 1000 {
            warn!("文件夹名冲突解决失败，使用随机后缀");
            unique_name = format!(
                "{}-{}",
                base_name,
                uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("random")
            );
            return unique_name;
        }
    }
}

/// 智能重组视频文件夹
/// 处理从共享文件夹（如按UP主分类）到独立文件夹（如按视频标题分类）的重组
// 从数据库查询并移动特定视频的所有文件到目标文件夹
async fn extract_video_files_by_database(
    db: &DatabaseConnection,
    video_id: i32,
    target_path: &std::path::Path,
) -> Result<(), std::io::Error> {
    use bili_sync_entity::prelude::*;
    use sea_orm::*;

    info!(
        "开始通过数据库查询移动视频文件到: {:?} (video_id: {})",
        target_path, video_id
    );

    // 创建目标文件夹
    std::fs::create_dir_all(target_path)?;

    // 首先获取视频信息以了解原始根目录
    info!("🔍 开始查询视频信息: video_id={}", video_id);
    let video = match Video::find_by_id(video_id).one(db).await {
        Ok(Some(v)) => {
            info!("✅ 成功获取视频信息: id={}, name={}, path={}", v.id, v.name, v.path);
            v
        }
        Ok(None) => {
            error!("❌ 视频不存在: video_id={}", video_id);
            return Err(std::io::Error::other(format!("视频 {} 不存在", video_id)));
        }
        Err(e) => {
            error!("❌ 数据库查询视频信息失败: video_id={}, 错误: {}", video_id, e);
            return Err(std::io::Error::other(format!("获取视频信息失败: {}", e)));
        }
    };

    let video_root_path = std::path::Path::new(&video.path);
    info!("📁 视频根目录: {:?}", video_root_path);
    info!("🎯 目标路径: {:?}", target_path);

    // 从数据库查询所有相关页面的文件路径
    info!("🔍 开始查询视频的所有页面: video_id={}", video_id);
    let pages = match Page::find()
        .filter(bili_sync_entity::page::Column::VideoId.eq(video_id))
        .filter(bili_sync_entity::page::Column::DownloadStatus.gt(0))
        .all(db)
        .await
    {
        Ok(pages) => {
            info!("✅ 成功查询到 {} 个已下载的页面", pages.len());
            for (idx, page) in pages.iter().enumerate() {
                info!(
                    "   页面 {}: id={}, name={}, path={:?}, download_status={}",
                    idx + 1,
                    page.id,
                    page.name,
                    page.path,
                    page.download_status
                );
            }
            pages
        }
        Err(e) => {
            error!("❌ 数据库查询页面失败: video_id={}, 错误: {}", video_id, e);
            return Err(std::io::Error::other(format!("数据库查询失败: {}", e)));
        }
    };

    if pages.is_empty() {
        warn!("⚠️ 视频 {} 没有已下载的页面，跳过处理", video_id);
        return Ok(());
    }

    let mut moved_files = 0;
    let mut total_files = 0;
    let mut pages_to_update = Vec::new(); // 记录需要更新路径的页面
    let mut source_dirs_to_check = std::collections::HashSet::new(); // 记录需要检查是否为空的源目录

    // 移动每个页面的相关文件
    info!("🔄 开始处理 {} 个页面的文件移动", pages.len());
    for (page_idx, page) in pages.iter().enumerate() {
        info!(
            "📄 处理页面 {}/{}: id={}, name={}",
            page_idx + 1,
            pages.len(),
            page.id,
            page.name
        );

        // 跳过没有路径信息的页面
        let page_path_str = match &page.path {
            Some(path) => {
                info!("   📍 页面路径: {}", path);
                path
            }
            None => {
                warn!("   ⚠️ 页面 {} 没有路径信息，跳过", page.id);
                continue;
            }
        };

        let page_file_path = std::path::Path::new(page_path_str);
        info!("   🔍 检查页面文件: {:?}", page_file_path);

        // 获取页面文件所在的目录
        if let Some(page_dir) = page_file_path.parent() {
            info!("   📁 页面所在目录: {:?}", page_dir);
            // 记录源目录，稍后检查是否需要删除
            source_dirs_to_check.insert(page_dir.to_path_buf());
            // 收集该页面的所有相关文件
            match std::fs::read_dir(page_dir) {
                Ok(entries) => {
                    info!("   ✅ 成功读取目录，开始扫描文件");
                    for entry in entries.flatten() {
                        let file_path = entry.path();

                        // 检查文件是否属于当前页面
                        if let Some(file_name) = file_path.file_name() {
                            let file_name_str = file_name.to_string_lossy();
                            let page_base_name = page_file_path.file_stem().unwrap_or_default().to_string_lossy();

                            // 获取原始基础名称（去除数字后缀）
                            let original_base_name = if let Some(index) = page_base_name.rfind('-') {
                                if let Some(suffix) = page_base_name.get(index + 1..) {
                                    if suffix.chars().all(|c| c.is_ascii_digit()) {
                                        // 如果后缀是纯数字，说明是重复文件，使用原始名称匹配
                                        page_base_name.get(..index).unwrap_or(&page_base_name)
                                    } else {
                                        &page_base_name
                                    }
                                } else {
                                    &page_base_name
                                }
                            } else {
                                &page_base_name
                            };

                            // 如果文件名包含原始基础名称，就认为是相关文件
                            if file_name_str.contains(original_base_name) {
                                total_files += 1;
                                info!(
                                    "       📎 找到相关文件: {:?} (匹配基础名: {})",
                                    file_path, original_base_name
                                );

                                // **关键修复：计算文件相对于视频根目录的路径**
                                let relative_path = if let Ok(rel_path) = file_path.strip_prefix(video_root_path) {
                                    let rel_parent = rel_path.parent().unwrap_or(std::path::Path::new(""));
                                    info!("       📐 计算相对路径成功: {:?} -> {:?}", file_path, rel_parent);
                                    rel_parent
                                } else {
                                    info!("       ⚠️ 无法使用strip_prefix计算相对路径，尝试备用方法");
                                    // 如果无法计算相对路径，至少保持文件所在的直接父目录
                                    if let (Some(file_parent), Some(video_parent)) =
                                        (file_path.parent(), video_root_path.parent())
                                    {
                                        if let Ok(rel) = file_parent.strip_prefix(video_parent) {
                                            info!("       📐 备用方法计算相对路径成功: {:?}", rel);
                                            rel
                                        } else {
                                            info!("       📐 备用方法也无法计算相对路径，使用空路径");
                                            std::path::Path::new("")
                                        }
                                    } else {
                                        info!("       📐 无法获取父目录，使用空路径");
                                        std::path::Path::new("")
                                    }
                                };

                                // **关键修复：在目标路径中保持相对目录结构**
                                let target_dir = target_path.join(relative_path);
                                let target_file = target_dir.join(file_name);
                                info!("       🎯 目标目录: {:?}", target_dir);
                                info!("       🎯 目标文件: {:?}", target_file);

                                // 确保目标子目录存在
                                if !target_dir.exists() {
                                    info!("       📁 创建目标子目录: {:?}", target_dir);
                                    if let Err(e) = std::fs::create_dir_all(&target_dir) {
                                        error!("       ❌ 创建目标子目录失败: {:?}, 错误: {}", target_dir, e);
                                        continue;
                                    }
                                    info!("       ✅ 目标子目录创建成功");
                                } else {
                                    info!("       ✅ 目标子目录已存在");
                                }

                                // 避免重复移动（如果文件已经在目标位置）
                                if file_path == target_file {
                                    info!("       ↩️ 文件已在目标位置，跳过: {:?}", file_path);
                                    continue;
                                }

                                // 如果目标文件已存在，生成新的文件名避免覆盖
                                let final_target_file = if target_file.exists() {
                                    warn!("       ⚠️ 目标文件已存在，生成唯一文件名: {:?}", target_file);
                                    let unique_file =
                                        generate_unique_filename_with_video_info(&target_file, video_id, db).await;
                                    info!("       🔄 生成唯一文件名: {:?}", unique_file);
                                    unique_file
                                } else {
                                    target_file.clone()
                                };

                                info!("       🚀 开始移动文件: {:?} -> {:?}", file_path, final_target_file);
                                match std::fs::rename(&file_path, &final_target_file) {
                                    Ok(_) => {
                                        moved_files += 1;
                                        info!("       ✅ 文件移动成功 (总计: {}/{})", moved_files, total_files);

                                        // **关键修复：如果移动的是页面主文件，记录需要更新数据库路径**
                                        // 检查是否为主文件：mp4或nfo文件，且文件名匹配原始基础名称
                                        let is_main_file = if let Some(extension) = file_path.extension() {
                                            let ext_str = extension.to_string_lossy().to_lowercase();
                                            (ext_str == "mp4" || ext_str == "nfo")
                                                && file_name_str.starts_with(original_base_name)
                                                && !file_name_str.contains("-fanart")
                                                && !file_name_str.contains("-poster")
                                                && !file_name_str.contains(".zh-CN.default")
                                        } else {
                                            false
                                        };

                                        if is_main_file {
                                            pages_to_update
                                                .push((page.id, final_target_file.to_string_lossy().to_string()));
                                            info!(
                                                "       🎯 页面主文件移动成功，将更新数据库路径: {:?} -> {:?}",
                                                file_path, final_target_file
                                            );
                                        } else if final_target_file != target_file {
                                            info!(
                                                "       🔄 移动文件成功（重命名避免覆盖）: {:?} -> {:?}",
                                                file_path, final_target_file
                                            );
                                        } else {
                                            info!("       ✅ 移动文件成功: {:?} -> {:?}", file_path, final_target_file);
                                        }
                                    }
                                    Err(e) => {
                                        error!(
                                            "       ❌ 移动文件失败: {:?} -> {:?}, 错误: {}",
                                            file_path, final_target_file, e
                                        );
                                    }
                                }
                            } else {
                                debug!(
                                    "       🔍 文件不匹配基础名，跳过: {:?} (基础名: {})",
                                    file_path, original_base_name
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("   ❌ 无法读取目录 {:?}: {}", page_dir, e);
                    continue;
                }
            }
        }
    }

    // **关键修复：批量更新数据库中的页面路径**
    if !pages_to_update.is_empty() {
        info!("💾 开始更新 {} 个页面的数据库路径", pages_to_update.len());

        for (page_id, new_path) in pages_to_update {
            info!("   💾 更新页面 {} 的路径: {}", page_id, new_path);
            match Page::update_many()
                .filter(bili_sync_entity::page::Column::Id.eq(page_id))
                .col_expr(bili_sync_entity::page::Column::Path, Expr::value(new_path.clone()))
                .exec(db)
                .await
            {
                Ok(_) => {
                    info!("   ✅ 更新页面 {} 的数据库路径成功", page_id);
                }
                Err(e) => {
                    error!("   ❌ 更新页面 {} 的数据库路径失败: {}, 错误: {}", page_id, new_path, e);
                }
            }
        }

        info!("💾 页面数据库路径更新完成");
    }

    // **新增修复：扫描和移动视频根目录中的元数据文件**
    info!("📂 开始扫描视频根目录的元数据文件: {:?}", video_root_path);
    if video_root_path.exists() && video_root_path.is_dir() {
        match std::fs::read_dir(video_root_path) {
            Ok(entries) => {
                info!("✅ 成功读取视频根目录，开始扫描元数据文件");
                for entry in entries.flatten() {
                    let file_path = entry.path();
                    if file_path.is_file() {
                        if let Some(file_name) = file_path.file_name() {
                            let file_name_str = file_name.to_string_lossy();

                            // 检查是否为视频级元数据文件
                            let is_video_metadata = file_name_str == "tvshow.nfo"
                                || file_name_str.ends_with("-fanart.jpg")
                                || file_name_str.ends_with("-thumb.jpg")
                                || file_name_str.ends_with(".nfo");

                            if is_video_metadata {
                                total_files += 1;
                                info!("   📎 找到视频级元数据文件: {:?}", file_path);

                                // 视频级元数据文件直接移动到目标根目录
                                let target_file = target_path.join(file_name);
                                info!("   🎯 目标文件: {:?}", target_file);

                                // 检查目标文件是否已存在，如果存在则重命名
                                let final_target_file = if target_file.exists() {
                                    let base_name = target_file.file_stem().unwrap_or_default().to_string_lossy();
                                    let extension = target_file
                                        .extension()
                                        .map(|e| format!(".{}", e.to_string_lossy()))
                                        .unwrap_or_default();
                                    let counter_file = target_path.join(format!("{}-1{}", base_name, extension));
                                    info!("   ⚠️ 目标文件已存在，重命名为: {:?}", counter_file);
                                    counter_file
                                } else {
                                    target_file
                                };

                                // 移动文件
                                info!(
                                    "   🚀 开始移动视频级元数据文件: {:?} -> {:?}",
                                    file_path, final_target_file
                                );
                                match std::fs::rename(&file_path, &final_target_file) {
                                    Ok(_) => {
                                        moved_files += 1;
                                        info!("   ✅ 视频级元数据文件移动成功 (总计: {}/{})", moved_files, total_files);
                                        info!("   ✅ 移动文件成功: {:?} -> {:?}", file_path, final_target_file);
                                    }
                                    Err(e) => {
                                        error!(
                                            "   ❌ 移动视频级元数据文件失败: {:?} -> {:?}, 错误: {}",
                                            file_path, final_target_file, e
                                        );
                                    }
                                }
                            } else {
                                debug!("   🔍 跳过非元数据文件: {:?}", file_path);
                            }
                        }
                    }
                }

                // 添加视频根目录到清理检查列表
                source_dirs_to_check.insert(video_root_path.to_path_buf());
                info!("   📝 已添加视频根目录到清理检查列表: {:?}", video_root_path);
            }
            Err(e) => {
                warn!("❌ 无法读取视频根目录 {:?}: {}", video_root_path, e);
            }
        }
    } else {
        info!("⚠️ 视频根目录不存在或不是目录: {:?}", video_root_path);
    }

    // **清理空的源文件夹**
    info!("🧹 开始清理空的源文件夹，检查 {} 个目录", source_dirs_to_check.len());
    let mut cleaned_dirs = 0;
    for source_dir in source_dirs_to_check {
        info!("   🔍 检查源目录: {:?}", source_dir);
        // 跳过目标路径，避免删除新创建的文件夹
        if source_dir == target_path {
            info!("   ↩️ 跳过目标路径，避免删除新创建的文件夹");
            continue;
        }

        // 检查目录是否为空
        match std::fs::read_dir(&source_dir) {
            Ok(entries) => {
                let remaining_files: Vec<_> = entries.flatten().collect();
                if remaining_files.is_empty() {
                    info!("   📁 目录为空，尝试删除: {:?}", source_dir);
                    // 目录为空，尝试删除
                    match std::fs::remove_dir(&source_dir) {
                        Ok(_) => {
                            cleaned_dirs += 1;
                            info!("   ✅ 删除空文件夹成功: {:?}", source_dir);
                        }
                        Err(e) => {
                            warn!("   ❌ 删除空文件夹失败: {:?}, 错误: {}", source_dir, e);
                        }
                    }
                } else {
                    info!(
                        "   📄 源文件夹仍有 {} 个文件，保留: {:?}",
                        remaining_files.len(),
                        source_dir
                    );
                }
            }
            Err(e) => {
                warn!("   ❌ 无法读取源目录: {:?}, 错误: {}", source_dir, e);
            }
        }
    }

    if cleaned_dirs > 0 {
        info!("🧹 清理完成：删除了 {} 个空文件夹", cleaned_dirs);
    } else {
        info!("🧹 清理完成：没有空文件夹需要删除");
    }

    info!(
        "🎉 视频 {} 文件移动完成: 成功移动 {}/{} 个文件到 {:?}",
        video_id, moved_files, total_files, target_path
    );

    if moved_files == 0 && total_files > 0 {
        warn!(
            "⚠️ 发现了 {} 个文件但没有移动任何文件，请检查权限或路径问题",
            total_files
        );
    } else if moved_files == 0 {
        warn!("⚠️ 没有找到任何相关文件进行移动");
    }

    Ok(())
}

// 根据视频ID生成唯一文件名（使用发布时间或BVID后缀）
async fn generate_unique_filename_with_video_info(
    target_file: &std::path::Path,
    video_id: i32,
    db: &DatabaseConnection,
) -> std::path::PathBuf {
    let file_stem = target_file.file_stem().unwrap_or_default().to_string_lossy();
    let file_extension = target_file.extension().unwrap_or_default().to_string_lossy();
    let parent_dir = target_file.parent().unwrap_or(std::path::Path::new(""));

    // 尝试从数据库获取视频信息来生成更有意义的后缀
    let suffix = if let Ok(Some(video)) = video::Entity::find_by_id(video_id).one(db).await {
        // 优先使用发布时间
        format!("{}", video.pubtime.format("%Y-%m-%d"))
    } else {
        format!("vid{}", video_id)
    };

    let new_name = if file_extension.is_empty() {
        format!("{}-{}", file_stem, suffix)
    } else {
        format!("{}-{}.{}", file_stem, suffix, file_extension)
    };
    let new_target = parent_dir.join(new_name);

    // 如果仍然冲突，添加时间戳
    if new_target.exists() {
        let timestamp = chrono::Local::now().format("%H%M%S").to_string();
        let final_name = if file_extension.is_empty() {
            format!("{}-{}-{}", file_stem, suffix, timestamp)
        } else {
            format!("{}-{}-{}.{}", file_stem, suffix, timestamp, file_extension)
        };
        parent_dir.join(final_name)
    } else {
        new_target
    }
}

/// 更新番剧视频在数据库中的路径（不移动文件，只更新数据库）
async fn update_bangumi_video_path_in_database(
    txn: &sea_orm::DatabaseTransaction,
    video: &video::Model,
    new_base_path: &str,
) -> Result<(), ApiError> {
    use std::path::Path;

    // 计算该视频的新路径（与move_bangumi_files_to_new_path使用相同逻辑）
    let new_video_dir = Path::new(new_base_path);

    // 基于视频模型重新生成路径结构（使用番剧专用逻辑）
    let new_video_path = if video.source_type == Some(1) {
        // 番剧使用专用的路径计算逻辑，与workflow.rs保持一致

        // 创建临时page模型用于格式化参数
        let temp_page = bili_sync_entity::page::Model {
            id: 0,
            video_id: video.id,
            cid: 0,
            pid: 1,
            name: "temp".to_string(),
            width: None,
            height: None,
            duration: 0,
            path: None,
            image: None,
            download_status: 0,
            created_at: now_standard_string(),
            play_video_streams: None,
            play_audio_streams: None,
            play_subtitle_streams: None,
            play_streams_updated_at: None,
            ai_renamed: None,
        };

        // 🚨 修复路径提取逻辑：处理混合路径分隔符问题
        // 数据库中的路径可能包含混合的路径分隔符，如：D:/Downloads/00111\名侦探柯南 绝海的侦探
        let api_title = {
            debug!("=== 数据库路径更新调试 ===");
            debug!("视频ID: {}, BVID: {}", video.id, video.bvid);
            debug!("视频名称: {}", video.name);
            debug!("原始数据库路径: {}", &video.path);
            debug!("新基础路径: {}", new_base_path);

            // 🔧 标准化路径分隔符：统一转换为当前平台的分隔符
            let normalized_path = video.path.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR);
            debug!("标准化后的路径: {}", normalized_path);

            // 🔍 从标准化路径中提取番剧文件夹名称
            let current_path = std::path::Path::new(&normalized_path);
            debug!("Path组件: {:?}", current_path.components().collect::<Vec<_>>());

            let path_extracted = current_path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string());
            debug!("从标准化路径提取的文件夹名: {:?}", path_extracted);

            // ✅ 验证提取的名称是否合理（包含中文字符或非纯数字）
            if let Some(ref name) = path_extracted {
                let is_likely_bangumi_name = !name.chars().all(|c| c.is_ascii_digit()) && name.len() > 3; // 番剧名通常比较长

                if is_likely_bangumi_name {
                    debug!("✅ 提取的番剧文件夹名看起来合理: '{}'", name);
                    path_extracted
                } else {
                    debug!("⚠️ 提取的名称 '{}' 看起来不像番剧名（可能是根目录）", name);
                    debug!("💡 将使用None来触发模板的默认行为");
                    None
                }
            } else {
                debug!("❌ 无法从路径中提取文件夹名");
                None
            }
        };

        // 使用番剧格式化参数生成正确的番剧文件夹路径
        let format_args = crate::utils::format_arg::bangumi_page_format_args(video, &temp_page, api_title.as_deref());
        debug!(
            "格式化参数: {}",
            serde_json::to_string_pretty(&format_args).unwrap_or_default()
        );

        // 检查是否有有效的series_title
        let series_title = format_args["series_title"].as_str().unwrap_or("");
        debug!("提取的series_title: '{}'", series_title);

        if series_title.is_empty() {
            return Err(anyhow!(
                "番剧 {} (BVID: {}) 缺少有效的系列标题，无法生成路径",
                video.name,
                video.bvid
            )
            .into());
        }

        // 生成番剧文件夹名称
        let rendered_folder = crate::config::with_config(|bundle| bundle.render_bangumi_folder_template(&format_args))
            .map_err(|e| anyhow!("番剧文件夹模板渲染失败: {}", e))?;

        debug!("渲染的番剧文件夹名: '{}'", rendered_folder);
        rendered_folder
    } else {
        return Err(anyhow!("非番剧视频不应调用此函数").into());
    };

    let target_video_dir = new_video_dir.join(&new_video_path);
    debug!("=== 最终路径构建 ===");
    debug!("新基础目录: {:?}", new_video_dir);
    debug!("生成的番剧文件夹名: '{}'", new_video_path);
    debug!("最终目标路径: {:?}", target_video_dir);

    // 只更新数据库，不移动文件
    let video_path_str = target_video_dir.to_string_lossy().to_string();
    debug!("将要保存到数据库的路径字符串: '{}'", video_path_str);

    video::Entity::update_many()
        .filter(video::Column::Id.eq(video.id))
        .col_expr(video::Column::Path, Expr::value(video_path_str.clone()))
        .exec(txn)
        .await?;

    info!(
        "更新番剧视频 {} 数据库路径: {} -> {}",
        video.id, video.path, video_path_str
    );
    Ok(())
}

/// 番剧专用的文件移动函数，避免BVID后缀污染
async fn move_bangumi_files_to_new_path(
    video: &video::Model,
    _old_base_path: &str,
    new_base_path: &str,
    clean_empty_folders: bool,
    txn: &sea_orm::DatabaseTransaction,
) -> Result<(usize, usize), std::io::Error> {
    use std::path::Path;

    let mut moved_count = 0;
    let mut cleaned_count = 0;

    // 获取当前视频的存储路径
    let current_video_path = Path::new(&video.path);
    if !current_video_path.exists() {
        return Ok((0, 0)); // 如果视频文件夹不存在，跳过
    }

    // 使用模板重新生成视频在新基础路径下的目标路径
    let new_video_dir = Path::new(new_base_path);

    // 基于视频模型重新生成路径结构（使用番剧专用逻辑）
    let new_video_path = if video.source_type == Some(1) {
        // 番剧使用专用的路径计算逻辑，与workflow.rs保持一致

        // 创建临时page模型用于格式化参数
        let temp_page = bili_sync_entity::page::Model {
            id: 0,
            video_id: video.id,
            cid: 0,
            pid: 1,
            name: "temp".to_string(),
            width: None,
            height: None,
            duration: 0,
            path: None,
            image: None,
            download_status: 0,
            created_at: now_standard_string(),
            play_video_streams: None,
            play_audio_streams: None,
            play_subtitle_streams: None,
            play_streams_updated_at: None,
            ai_renamed: None,
        };

        // 修复路径提取逻辑：处理混合路径分隔符问题
        // 数据库中的路径可能包含混合的路径分隔符，如：D:/Downloads/00111\名侦探柯南 绝海的侦探
        let api_title = {
            // 标准化路径分隔符：统一转换为当前平台的分隔符
            let normalized_path = video.path.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR);

            // 从标准化路径中提取番剧文件夹名称
            let current_path = std::path::Path::new(&normalized_path);
            let path_extracted = current_path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string());

            // 验证提取的名称是否合理（包含中文字符或非纯数字）
            if let Some(ref name) = path_extracted {
                let is_likely_bangumi_name = !name.chars().all(|c| c.is_ascii_digit()) && name.len() > 3; // 番剧名通常比较长

                if is_likely_bangumi_name {
                    path_extracted
                } else {
                    None // 使用None来触发模板的默认行为
                }
            } else {
                None
            }
        };

        // 使用番剧格式化参数生成正确的番剧文件夹路径
        let format_args = crate::utils::format_arg::bangumi_page_format_args(video, &temp_page, api_title.as_deref());

        // 检查是否有有效的series_title
        let series_title = format_args["series_title"].as_str().unwrap_or("");

        if series_title.is_empty() {
            return Err(std::io::Error::other(format!(
                "番剧 {} (BVID: {}) 缺少有效的系列标题，无法生成路径",
                video.name, video.bvid
            )));
        }

        // 生成番剧文件夹名称
        let rendered_folder = crate::config::with_config(|bundle| bundle.render_bangumi_folder_template(&format_args))
            .map_err(|e| std::io::Error::other(format!("番剧文件夹模板渲染失败: {}", e)))?;

        rendered_folder
    } else {
        // 非番剧使用原有逻辑
        crate::config::with_config(|bundle| {
            let video_args = crate::utils::format_arg::video_format_args(video);
            bundle.render_video_template(&video_args)
        })
        .map_err(|e| std::io::Error::other(format!("模板渲染失败: {}", e)))?
    };

    let target_video_dir = new_video_dir.join(&new_video_path);

    // 如果目标路径和当前路径相同，无需移动
    if current_video_path == target_video_dir {
        return Ok((0, 0));
    }

    // 使用四步重命名原则移动整个视频文件夹
    if (move_files_with_four_step_rename(
        &current_video_path.to_string_lossy(),
        &target_video_dir.to_string_lossy(),
    )
    .await)
        .is_ok()
    {
        moved_count = 1;

        // 移动成功后，执行番剧专用的文件重命名
        if let Err(e) = rename_bangumi_files_in_directory(&target_video_dir, video, txn).await {
            warn!("番剧文件重命名失败: {}", e);
        }

        // 移动成功后，检查并清理原来的父目录（如果启用了清理且为空）
        if clean_empty_folders {
            if let Some(parent_dir) = current_video_path.parent() {
                if let Ok(count) = cleanup_empty_directory(parent_dir).await {
                    cleaned_count = count;
                }
            }
        }
    }

    Ok((moved_count, cleaned_count))
}

/// 番剧文件重命名：只重命名集数部分，保留版本和后缀
async fn rename_bangumi_files_in_directory(
    video_dir: &std::path::Path,
    video: &video::Model,
    txn: &sea_orm::DatabaseTransaction,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    // 读取视频文件夹中的所有文件
    let entries = fs::read_dir(video_dir)?;

    // 获取相关分页信息
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(video.id))
        .all(txn)
        .await?;

    for entry in entries {
        let entry = entry?;
        let file_path = entry.path();

        if !file_path.is_file() {
            continue;
        }

        let old_file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();

        // 解析并重命名番剧文件
        if let Some(new_file_name) = parse_and_rename_bangumi_file(&old_file_name, video, &pages) {
            if new_file_name != old_file_name {
                let new_file_path = video_dir.join(&new_file_name);

                match fs::rename(&file_path, &new_file_path) {
                    Ok(_) => {
                        debug!("番剧文件重命名成功: {} -> {}", old_file_name, new_file_name);

                        // 如果是MP4文件，更新数据库中的分页路径
                        if new_file_name.ends_with(".mp4") {
                            update_page_path_in_database(txn, &pages, &new_file_name, &new_file_path).await?;
                        }
                    }
                    Err(e) => {
                        warn!(
                            "番剧文件重命名失败: {} -> {}, 错误: {}",
                            old_file_name, new_file_name, e
                        );
                    }
                }
            }
        }
    }

    // 注意：数据库路径更新现在由调用方统一处理，避免多版本视频路径冲突
    Ok(())
}

/// 解析番剧文件名并重新组合
fn parse_and_rename_bangumi_file(old_file_name: &str, video: &video::Model, pages: &[page::Model]) -> Option<String> {
    // 尝试匹配各种番剧文件名模式

    // 1. NFO信息文件 (需要支持重置重新生成)
    if matches!(old_file_name, "tvshow.nfo") {
        return Some(old_file_name.to_string()); // NFO文件保持原名但支持重置
    }

    // 2. 媒体文件 (不需要重新生成)
    if matches!(old_file_name, "thumb.jpg" | "fanart.jpg") {
        return Some(old_file_name.to_string()); // 这些文件不需要重命名
    }

    // 3. 分页相关文件模式匹配
    // 支持的格式：S01E01-中配.mp4, S01E01-中配-thumb.jpg, 第1集-日配-fanart.jpg 等
    if let Some((episode_part, suffix)) = parse_episode_file_name(old_file_name) {
        // 重新生成集数格式
        if let Some(new_episode_format) = generate_new_episode_format(video, pages, &episode_part) {
            return Some(format!("{}{}", new_episode_format, suffix));
        }
    }

    None
}

/// 解析文件名中的集数部分和后缀
fn parse_episode_file_name(file_name: &str) -> Option<(String, String)> {
    // 匹配模式：S01E01-版本-类型.扩展名 或 第X集-版本-类型.扩展名

    // 匹配 SxxExx 格式
    if let Some(captures) = regex::Regex::new(r"^(S\d{2}E\d{2})(.*)$").ok()?.captures(file_name) {
        let episode_part = captures.get(1)?.as_str().to_string();
        let suffix = captures.get(2)?.as_str().to_string();
        return Some((episode_part, suffix));
    }

    // 匹配 第X集 格式
    if let Some(captures) = regex::Regex::new(r"^(第\d+集)(.*)$").ok()?.captures(file_name) {
        let episode_part = captures.get(1)?.as_str().to_string();
        let suffix = captures.get(2)?.as_str().to_string();
        return Some((episode_part, suffix));
    }

    None
}

/// 生成新的集数格式
fn generate_new_episode_format(video: &video::Model, pages: &[page::Model], _old_episode_part: &str) -> Option<String> {
    // 如果是多P视频，使用第一个分页的信息生成新格式
    if let Some(first_page) = pages.first() {
        // 使用配置中的分页模板生成新的集数格式
        if let Ok(new_format) = crate::config::with_config(|bundle| {
            let page_args = crate::utils::format_arg::page_format_args(video, first_page);
            bundle.render_page_template(&page_args)
        }) {
            return Some(new_format);
        }
    }

    // 后备方案：使用集数信息生成
    if let Some(episode_number) = video.episode_number {
        return Some(format!("第{:02}集", episode_number));
    }

    None
}

/// 更新数据库中的分页路径
async fn update_page_path_in_database(
    txn: &sea_orm::DatabaseTransaction,
    pages: &[page::Model],
    new_file_name: &str,
    new_file_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // 查找匹配的分页记录并更新其路径
    for page_model in pages {
        // 简单匹配：如果新文件名包含分页标题或PID信息，则更新该分页的路径
        if new_file_name.contains(&page_model.name) || new_file_name.contains(&page_model.pid.to_string()) {
            page::Entity::update_many()
                .filter(page::Column::Id.eq(page_model.id))
                .col_expr(
                    page::Column::Path,
                    Expr::value(Some(new_file_path.to_string_lossy().to_string())),
                )
                .exec(txn)
                .await?;
            break;
        }
    }

    Ok(())
}

/// 验证收藏夹ID并获取收藏夹信息
#[utoipa::path(
    get,
    path = "/api/favorite/{fid}/validate",
    params(
        ("fid" = String, Path, description = "收藏夹ID"),
    ),
    responses(
        (status = 200, body = ApiResponse<crate::api::response::ValidateFavoriteResponse>),
    )
)]
pub async fn validate_favorite(
    Path(fid): Path<String>,
) -> Result<ApiResponse<crate::api::response::ValidateFavoriteResponse>, ApiError> {
    // 创建B站客户端
    let client = crate::bilibili::BiliClient::new(String::new());

    // 创建收藏夹对象
    let favorite_list = crate::bilibili::FavoriteList::new(&client, fid.clone());

    // 尝试获取收藏夹信息
    match favorite_list.get_info().await {
        Ok(info) => Ok(ApiResponse::ok(crate::api::response::ValidateFavoriteResponse {
            valid: true,
            fid: info.id,
            title: info.title,
            message: "收藏夹验证成功".to_string(),
        })),
        Err(e) => {
            warn!("验证收藏夹 {} 失败: {}", fid, e);
            Ok(ApiResponse::ok(crate::api::response::ValidateFavoriteResponse {
                valid: false,
                fid: fid.parse().unwrap_or(0),
                title: String::new(),
                message: format!("收藏夹验证失败: 可能是ID不存在或收藏夹不公开。错误详情: {}", e),
            }))
        }
    }
}

/// 获取指定UP主的收藏夹列表
#[utoipa::path(
    get,
    path = "/api/user/{uid}/favorites",
    params(
        ("uid" = i64, Path, description = "UP主ID"),
    ),
    responses(
        (status = 200, body = ApiResponse<Vec<crate::api::response::UserFavoriteFolder>>),
    )
)]
pub async fn get_user_favorites_by_uid(
    Path(uid): Path<i64>,
) -> Result<ApiResponse<Vec<crate::api::response::UserFavoriteFolder>>, ApiError> {
    // 创建B站客户端
    let client = crate::bilibili::BiliClient::new(String::new());

    // 获取指定UP主的收藏夹列表
    match client.get_user_favorite_folders(Some(uid)).await {
        Ok(folders) => {
            let response_folders: Vec<crate::api::response::UserFavoriteFolder> = folders
                .into_iter()
                .map(|f| crate::api::response::UserFavoriteFolder {
                    id: f.id,
                    fid: f.fid,
                    title: f.title,
                    media_count: f.media_count,
                })
                .collect();

            Ok(ApiResponse::ok(response_folders))
        }
        Err(e) => {
            warn!("获取UP主 {} 的收藏夹失败: {}", uid, e);
            Err(crate::api::error::InnerApiError::BadRequest(format!(
                "获取UP主收藏夹失败: 可能是UP主不存在或收藏夹不公开。错误详情: {}",
                e
            ))
            .into())
        }
    }
}

/// 重置所有视频的NFO相关任务状态，用于配置更改后重新下载NFO文件
async fn reset_nfo_tasks_for_config_change(db: Arc<DatabaseConnection>) -> Result<(usize, usize)> {
    use sea_orm::*;
    use std::collections::HashSet;

    info!("开始重置NFO相关任务状态以应用新的配置...");

    // 根据配置决定是否过滤已删除的视频
    let scan_deleted = crate::config::with_config(|bundle| bundle.config.scan_deleted_videos);

    // 查询所有符合条件的视频
    let mut video_query = video::Entity::find();
    if !scan_deleted {
        video_query = video_query.filter(video::Column::Deleted.eq(0));
    }

    let all_videos = video_query
        .select_only()
        .columns([
            video::Column::Id,
            video::Column::Bvid,
            video::Column::Name,
            video::Column::UpperName,
            video::Column::Path,
            video::Column::Category,
            video::Column::DownloadStatus,
            video::Column::Cover,
            video::Column::Valid,
        ])
        .into_tuple::<(i32, String, String, String, String, i32, u32, String, bool)>()
        .all(db.as_ref())
        .await?;

    // 查询所有相关的页面
    let all_pages = page::Entity::find()
        .inner_join(video::Entity)
        .filter({
            let mut page_query_filter = sea_orm::Condition::all();
            if !scan_deleted {
                page_query_filter = page_query_filter.add(video::Column::Deleted.eq(0));
            }
            page_query_filter
        })
        .select_only()
        .columns([
            page::Column::Id,
            page::Column::Pid,
            page::Column::Name,
            page::Column::DownloadStatus,
            page::Column::VideoId,
        ])
        .into_tuple::<(i32, i32, String, u32, i32)>()
        .all(db.as_ref())
        .await?;

    // 重置页面的NFO任务状态（索引2：视频信息NFO）
    let resetted_pages_info = all_pages
        .into_iter()
        .filter_map(|(id, pid, name, download_status, video_id)| {
            let mut page_status = PageStatus::from(download_status);
            let current_nfo_status = page_status.get(2); // 索引2是视频信息NFO

            if current_nfo_status != 0 {
                // 只重置已经开始的NFO任务
                page_status.set(2, 0); // 重置为未开始
                let page_info = PageInfo::from((id, pid, name, page_status.into()));
                Some((page_info, video_id))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let video_ids_with_resetted_pages: HashSet<i32> =
        resetted_pages_info.iter().map(|(_, video_id)| *video_id).collect();

    let resetted_pages_info: Vec<PageInfo> = resetted_pages_info
        .into_iter()
        .map(|(page_info, _)| page_info)
        .collect();

    let all_videos_info: Vec<VideoInfo> = all_videos.into_iter().map(VideoInfo::from).collect();

    // 重置视频的NFO任务状态（索引1：视频信息NFO）
    let resetted_videos_info = all_videos_info
        .into_iter()
        .filter_map(|mut video_info| {
            let mut video_status = VideoStatus::from(video_info.download_status);
            let mut video_resetted = false;

            // 重置视频信息NFO任务（索引1）
            let current_nfo_status = video_status.get(1);
            if current_nfo_status != 0 {
                video_status.set(1, 0); // 重置为未开始
                video_resetted = true;
            }

            // 如果有页面被重置，同时重置分P下载状态（索引4）
            if video_ids_with_resetted_pages.contains(&video_info.id) {
                video_status.set(4, 0); // 将"分P下载"重置为 0
                video_resetted = true;
            }

            if video_resetted {
                video_info.download_status = video_status.into();
                Some(video_info)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let resetted = !(resetted_videos_info.is_empty() && resetted_pages_info.is_empty());

    if resetted {
        let txn = db.begin().await?;

        // 批量更新视频状态
        if !resetted_videos_info.is_empty() {
            for video in &resetted_videos_info {
                video::Entity::update(video::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(video.id),
                    download_status: sea_orm::Set(VideoStatus::from(video.download_status).into()),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        // 批量更新页面状态
        if !resetted_pages_info.is_empty() {
            for page in &resetted_pages_info {
                page::Entity::update(page::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(page.id),
                    download_status: sea_orm::Set(PageStatus::from(page.download_status).into()),
                    ..Default::default()
                })
                .exec(&txn)
                .await?;
            }
        }

        txn.commit().await?;
    }

    let resetted_videos_count = resetted_videos_info.len();
    let resetted_pages_count = resetted_pages_info.len();

    info!(
        "NFO任务状态重置完成，共重置了 {} 个视频和 {} 个页面的NFO任务",
        resetted_videos_count, resetted_pages_count
    );

    Ok((resetted_videos_count, resetted_pages_count))
}

/// 从全局缓存中获取番剧季标题
/// 如果缓存中没有，返回None（避免在API响应中阻塞）
async fn get_cached_season_title(season_id: &str) -> Option<String> {
    // 引用workflow模块中的全局缓存
    if let Ok(cache) = crate::workflow::SEASON_TITLE_CACHE.lock() {
        cache.get(season_id).cloned()
    } else {
        None
    }
}

/// 从API获取番剧标题并存入缓存
/// 这是一个轻量级实现，用于在API响应时补充缺失的标题
async fn fetch_and_cache_season_title(season_id: &str) -> Option<String> {
    let url = format!("https://api.bilibili.com/pgc/view/web/season?season_id={}", season_id);

    // 使用reqwest进行简单的HTTP请求
    let client = reqwest::Client::new();

    // 设置较短的超时时间，避免阻塞API响应
    match tokio::time::timeout(std::time::Duration::from_secs(3), client.get(&url).send()).await {
        Ok(Ok(response)) => {
            if response.status().is_success() {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    if json["code"].as_i64().unwrap_or(-1) == 0 {
                        if let Some(title) = json["result"]["title"].as_str() {
                            let title = title.to_string();

                            // 存入缓存
                            if let Ok(mut cache) = crate::workflow::SEASON_TITLE_CACHE.lock() {
                                cache.insert(season_id.to_string(), title.clone());
                                debug!("缓存番剧标题: {} -> {}", season_id, title);
                            }

                            return Some(title);
                        }
                    }
                }
            }
        }
        _ => {
            // 超时或请求失败，记录debug日志但不阻塞
            debug!("获取番剧标题超时: season_id={}", season_id);
        }
    }

    None
}

/// 获取仪表盘数据
#[utoipa::path(
    get,
    path = "/api/dashboard",
    responses(
        (status = 200, body = ApiResponse<DashBoardResponse>),
    ),
    security(
        ("auth_token" = [])
    )
)]
pub async fn get_dashboard_data(
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> Result<ApiResponse<crate::api::response::DashBoardResponse>, ApiError> {
    let (enabled_favorites, enabled_collections, enabled_submissions, enabled_watch_later, enabled_bangumi,
         total_favorites, total_collections, total_submissions, total_watch_later, total_bangumi, videos_by_day) = tokio::try_join!(
        favorite::Entity::find()
            .filter(favorite::Column::Enabled.eq(true))
            .count(db.as_ref()),
        collection::Entity::find()
            .filter(collection::Column::Enabled.eq(true))
            .count(db.as_ref()),
        submission::Entity::find()
            .filter(submission::Column::Enabled.eq(true))
            .count(db.as_ref()),
        watch_later::Entity::find()
            .filter(watch_later::Column::Enabled.eq(true))
            .count(db.as_ref()),
        video_source::Entity::find()
            .filter(video_source::Column::Type.eq(1))
            .filter(video_source::Column::Enabled.eq(true))
            .count(db.as_ref()),
        // 统计所有视频源（包括禁用的）
        favorite::Entity::find()
            .count(db.as_ref()),
        collection::Entity::find()
            .count(db.as_ref()),
        submission::Entity::find()
            .count(db.as_ref()),
        watch_later::Entity::find()
            .count(db.as_ref()),
        video_source::Entity::find()
            .filter(video_source::Column::Type.eq(1))
            .count(db.as_ref()),
        crate::api::response::DayCountPair::find_by_statement(sea_orm::Statement::from_string(
            db.get_database_backend(),
            // 用 SeaORM 太复杂了，直接写个裸 SQL
            // 修复时区处理：created_at 存储的是北京时间，直接使用日期比较
            "
SELECT
    dates.day AS day,
    COUNT(video.id) AS cnt
FROM
    (
        SELECT
            DATE('now', '-' || n || ' days', 'localtime') AS day
        FROM
            (
                SELECT 0 AS n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6
            )
    ) AS dates
LEFT JOIN
    video ON DATE(video.created_at) = dates.day
GROUP BY
    dates.day
ORDER BY
    dates.day;
    "
        ))
        .all(db.as_ref()),
    )?;

    // 获取监听状态信息
    let active_sources = enabled_favorites
        + enabled_collections
        + enabled_submissions
        + enabled_bangumi
        + if enabled_watch_later > 0 { 1 } else { 0 };
    let total_all_sources = total_favorites
        + total_collections
        + total_submissions
        + total_bangumi
        + if total_watch_later > 0 { 1 } else { 0 };
    let inactive_sources = total_all_sources - active_sources;

    // 从任务状态获取扫描时间信息
    let task_status = crate::utils::task_notifier::TASK_STATUS_NOTIFIER
        .subscribe()
        .borrow()
        .clone();
    let is_scanning = crate::task::TASK_CONTROLLER.is_scanning();

    let monitoring_status = MonitoringStatus {
        total_sources: total_all_sources,
        active_sources,
        inactive_sources,
        last_scan_time: task_status.last_run.map(to_standard_string),
        next_scan_time: task_status.next_run.map(to_standard_string),
        is_scanning,
    };

    Ok(ApiResponse::ok(crate::api::response::DashBoardResponse {
        enabled_favorites,
        enabled_collections,
        enabled_submissions,
        enabled_bangumi,
        enable_watch_later: enabled_watch_later > 0,
        total_favorites,
        total_collections,
        total_submissions,
        total_bangumi,
        total_watch_later,
        videos_by_day,
        monitoring_status,
    }))
}

/// 测试推送通知
#[utoipa::path(
    post,
    path = "/api/notification/test",
    request_body = crate::api::request::TestNotificationRequest,
    responses(
        (status = 200, description = "测试推送结果", body = ApiResponse<crate::api::response::TestNotificationResponse>),
        (status = 400, description = "配置错误", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn test_notification_handler(
    axum::Json(request): axum::Json<crate::api::request::TestNotificationRequest>,
) -> Result<ApiResponse<crate::api::response::TestNotificationResponse>, ApiError> {
    let config = crate::config::reload_config().notification;

    if !config.enable_scan_notifications {
        return Ok(ApiResponse::bad_request(
            crate::api::response::TestNotificationResponse {
                success: false,
                message: "推送通知功能未启用".to_string(),
            },
        ));
    }

    // 检查激活的渠道
    if config.active_channel == "none" {
        return Ok(ApiResponse::bad_request(
            crate::api::response::TestNotificationResponse {
                success: false,
                message: "未选择通知渠道".to_string(),
            },
        ));
    }

    // 验证选中渠道的配置
    match config.active_channel.as_str() {
        "serverchan" => {
            if config.serverchan_key.is_none() || config.serverchan_key.as_ref().unwrap().is_empty() {
                return Ok(ApiResponse::bad_request(
                    crate::api::response::TestNotificationResponse {
                        success: false,
                        message: "未配置Server酱密钥".to_string(),
                    },
                ));
            }
        }
        "wecom" => {
            if config.wecom_webhook_url.is_none() || config.wecom_webhook_url.as_ref().unwrap().is_empty() {
                return Ok(ApiResponse::bad_request(
                    crate::api::response::TestNotificationResponse {
                        success: false,
                        message: "未配置企业微信Webhook URL".to_string(),
                    },
                ));
            }
        }
        _ => {
            return Ok(ApiResponse::bad_request(
                crate::api::response::TestNotificationResponse {
                    success: false,
                    message: format!("未知的通知渠道: {}", config.active_channel),
                },
            ));
        }
    }

    let client = crate::utils::notification::NotificationClient::new(config);

    match if let Some(custom_msg) = request.custom_message {
        client.send_custom_test(&custom_msg).await
    } else {
        client.test_notification().await
    } {
        Ok(_) => Ok(ApiResponse::ok(crate::api::response::TestNotificationResponse {
            success: true,
            message: "测试推送发送成功".to_string(),
        })),
        Err(e) => Ok(ApiResponse::bad_request(
            crate::api::response::TestNotificationResponse {
                success: false,
                message: format!("推送发送失败: {}", e),
            },
        )),
    }
}

/// 获取推送配置
#[utoipa::path(
    get,
    path = "/api/config/notification",
    responses(
        (status = 200, description = "推送配置", body = ApiResponse<crate::api::response::NotificationConfigResponse>),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn get_notification_config() -> Result<ApiResponse<crate::api::response::NotificationConfigResponse>, ApiError>
{
    let mut config = crate::config::reload_config().notification;

    // 自动推断旧配置的激活渠道
    config.infer_active_channel();

    Ok(ApiResponse::ok(crate::api::response::NotificationConfigResponse {
        active_channel: config.active_channel,
        serverchan_key: config.serverchan_key,
        serverchan3_uid: config.serverchan3_uid,
        serverchan3_sendkey: config.serverchan3_sendkey,
        wecom_webhook_url: config.wecom_webhook_url,
        wecom_msgtype: config.wecom_msgtype,
        wecom_mention_all: config.wecom_mention_all,
        wecom_mentioned_list: config.wecom_mentioned_list,
        enable_scan_notifications: config.enable_scan_notifications,
        notification_min_videos: config.notification_min_videos,
        notification_timeout: config.notification_timeout,
        notification_retry_count: config.notification_retry_count,
    }))
}

/// 更新推送配置
#[utoipa::path(
    post,
    path = "/api/config/notification",
    request_body = crate::api::request::UpdateNotificationConfigRequest,
    responses(
        (status = 200, description = "配置更新成功", body = ApiResponse<String>),
        (status = 400, description = "配置验证失败", body = String),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn update_notification_config(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    axum::Json(request): axum::Json<crate::api::request::UpdateNotificationConfigRequest>,
) -> Result<ApiResponse<String>, ApiError> {
    use crate::config::ConfigManager;

    let config_manager = ConfigManager::new(db.as_ref().clone());

    // 先获取当前的notification配置
    let current_config = crate::config::reload_config();
    let mut notification_config = current_config.notification.clone();
    let mut updated = false;

    // 更新激活渠道
    if let Some(ref active_channel) = request.active_channel {
        notification_config.active_channel = active_channel.clone();
        updated = true;
    }

    // 更新配置字段
    if let Some(ref key) = request.serverchan_key {
        if key.trim().is_empty() {
            notification_config.serverchan_key = None;
        } else {
            notification_config.serverchan_key = Some(key.trim().to_string());
        }
        updated = true;
    }

    // Server酱3配置
    if let Some(ref uid) = request.serverchan3_uid {
        if uid.trim().is_empty() {
            notification_config.serverchan3_uid = None;
        } else {
            notification_config.serverchan3_uid = Some(uid.trim().to_string());
        }
        updated = true;
    }

    if let Some(ref sendkey) = request.serverchan3_sendkey {
        if sendkey.trim().is_empty() {
            notification_config.serverchan3_sendkey = None;
        } else {
            notification_config.serverchan3_sendkey = Some(sendkey.trim().to_string());
        }
        updated = true;
    }

    if let Some(ref url) = request.wecom_webhook_url {
        if url.trim().is_empty() {
            notification_config.wecom_webhook_url = None;
        } else {
            notification_config.wecom_webhook_url = Some(url.trim().to_string());
        }
        updated = true;
    }

    if let Some(ref msgtype) = request.wecom_msgtype {
        notification_config.wecom_msgtype = msgtype.clone();
        updated = true;
    }

    if let Some(mention_all) = request.wecom_mention_all {
        notification_config.wecom_mention_all = mention_all;
        updated = true;
    }

    if let Some(ref list) = request.wecom_mentioned_list {
        if list.is_empty() {
            notification_config.wecom_mentioned_list = None;
        } else {
            notification_config.wecom_mentioned_list = Some(list.clone());
        }
        updated = true;
    }

    if let Some(enabled) = request.enable_scan_notifications {
        notification_config.enable_scan_notifications = enabled;
        updated = true;
    }

    if let Some(min_videos) = request.notification_min_videos {
        if !(1..=100).contains(&min_videos) {
            return Err(ApiError::from(anyhow!("推送阈值必须在1-100之间")));
        }
        notification_config.notification_min_videos = min_videos;
        updated = true;
    }

    if let Some(timeout) = request.notification_timeout {
        if !(5..=60).contains(&timeout) {
            return Err(ApiError::from(anyhow!("超时时间必须在5-60秒之间")));
        }
        notification_config.notification_timeout = timeout;
        updated = true;
    }

    if let Some(retry_count) = request.notification_retry_count {
        if !(1..=5).contains(&retry_count) {
            return Err(ApiError::from(anyhow!("重试次数必须在1-5次之间")));
        }
        notification_config.notification_retry_count = retry_count;
        updated = true;
    }

    // 如果有更新，保存整个notification对象
    if updated {
        config_manager
            .update_config_item(
                "notification",
                serde_json::to_value(&notification_config)
                    .map_err(|e| ApiError::from(anyhow!("序列化通知配置失败: {}", e)))?,
            )
            .await
            .map_err(|e| ApiError::from(anyhow!("更新通知配置失败: {}", e)))?;
    }

    // 重新加载配置
    crate::config::reload_config_bundle()
        .await
        .map_err(|e| ApiError::from(anyhow!("重新加载配置失败: {}", e)))?;

    Ok(ApiResponse::ok("推送配置更新成功".to_string()))
}

/// 获取推送状态
#[utoipa::path(
    get,
    path = "/api/notification/status",
    responses(
        (status = 200, description = "推送状态", body = ApiResponse<crate::api::response::NotificationStatusResponse>),
        (status = 500, description = "服务器内部错误", body = String)
    )
)]
pub async fn get_notification_status() -> Result<ApiResponse<crate::api::response::NotificationStatusResponse>, ApiError>
{
    // 确保获取最新的配置
    if let Err(e) = crate::config::reload_config_bundle().await {
        warn!("重新加载配置失败: {}", e);
    }

    // 从当前配置包中获取最新的通知配置
    let config = crate::config::with_config(|bundle| bundle.config.notification.clone());

    // 这里可以从数据库或缓存中获取推送统计信息
    let status = crate::api::response::NotificationStatusResponse {
        configured: config.serverchan_key.is_some(),
        enabled: config.enable_scan_notifications,
        last_notification_time: None, // TODO: 从存储中获取
    };

    Ok(ApiResponse::ok(status))
}

/// 从番剧标题中提取系列名称
/// 例如：《灵笼 第二季》第1话 末世桃源 -> 灵笼
fn extract_bangumi_series_title(full_title: &str) -> String {
    // 移除开头的书名号
    let title = full_title.trim_start_matches('《');

    // 找到书名号结束位置
    if let Some(end_pos) = title.find('》') {
        let season_title = &title[..end_pos];

        // 移除季度信息："灵笼 第二季" -> "灵笼"
        if let Some(space_pos) = season_title.rfind(' ') {
            // 检查空格后面是否是季度标记
            let after_space = &season_title[space_pos + 1..];
            if after_space.starts_with("第") && after_space.ends_with("季") {
                return season_title[..space_pos].to_string();
            }
        }
        // 如果没有季度信息，返回整个标题
        return season_title.to_string();
    }

    // 如果没有书名号，尝试其他模式
    if let Some(space_pos) = full_title.find(' ') {
        return full_title[..space_pos].to_string();
    }

    full_title.to_string()
}

/// 从番剧标题中提取季度标题
/// 例如：《灵笼 第二季》第1话 末世桃源 -> 灵笼 第二季
fn extract_bangumi_season_title(full_title: &str) -> String {
    let title = full_title.trim_start_matches('《');

    if let Some(end_pos) = title.find('》') {
        return title[..end_pos].to_string();
    }

    // 如果没有书名号，找到"第X话"之前的部分
    if let Some(episode_pos) = full_title.find("第") {
        if let Some(hua_pos) = full_title[episode_pos..].find("话") {
            // 确保这是"第X话"而不是"第X季"
            let between = &full_title[episode_pos + 3..episode_pos + hua_pos];
            if between.chars().all(|c| c.is_numeric()) && episode_pos > 0 {
                return full_title[..episode_pos].trim().to_string();
            }
        }
    }

    full_title.to_string()
}

/// 从API获取合集封面URL
async fn get_collection_cover_from_api(
    up_id: i64,
    collection_id: i64,
    client: &crate::bilibili::BiliClient,
) -> Result<String, anyhow::Error> {
    // 分页获取所有合集，避免遗漏
    let mut page = 1;
    loop {
        let collections_response = client.get_user_collections(up_id, page, 50).await?;

        // 查找目标合集
        for collection in &collections_response.collections {
            if collection.sid.parse::<i64>().unwrap_or(0) == collection_id {
                if !collection.cover.is_empty() {
                    return Ok(collection.cover.clone());
                } else {
                    return Err(anyhow!("合集封面URL为空"));
                }
            }
        }

        // 检查是否还有更多页
        if collections_response.collections.len() < 50 {
            break; // 已经是最后一页
        }
        page += 1;

        // 安全限制，避免无限循环
        if page > 20 {
            return Err(anyhow!("搜索合集时达到最大页数限制 (20页)"));
        }
    }

    Err(anyhow!("未找到合集ID {} (UP主: {})", collection_id, up_id))
}

/// 处理番剧合并到现有源的逻辑
async fn handle_bangumi_merge_to_existing(
    txn: &sea_orm::DatabaseTransaction,
    params: AddVideoSourceRequest,
    merge_target_id: i32,
) -> Result<AddVideoSourceResponse, ApiError> {
    // 1. 查找目标番剧源
    let mut target_source = video_source::Entity::find_by_id(merge_target_id)
        .one(txn)
        .await?
        .ok_or_else(|| anyhow!("指定的目标番剧源不存在 (ID: {})", merge_target_id))?;

    // 验证目标确实是番剧类型
    if target_source.r#type != 1 {
        return Err(anyhow!("指定的目标不是番剧源").into());
    }

    // 2. 准备合并操作
    let download_all_seasons = params.download_all_seasons.unwrap_or(false);
    let mut updated = false;
    let mut merge_message = String::new();

    // 3. 处理季度合并逻辑
    if download_all_seasons {
        // 新请求要下载全部季度
        if !target_source.download_all_seasons.unwrap_or(false) {
            target_source.download_all_seasons = Some(true);
            target_source.selected_seasons = None; // 清空特定季度选择
            updated = true;
            merge_message = "已更新为下载全部季度".to_string();
        } else {
            merge_message = "目标番剧已配置为下载全部季度".to_string();
        }
    } else {
        // 处理特定季度的合并
        if let Some(new_seasons) = params.selected_seasons {
            if !new_seasons.is_empty() {
                let mut current_seasons: Vec<String> = Vec::new();

                // 获取现有的季度选择
                if let Some(ref seasons_json) = target_source.selected_seasons {
                    if let Ok(seasons) = serde_json::from_str::<Vec<String>>(seasons_json) {
                        current_seasons = seasons;
                    }
                }

                // 合并新的季度（去重）
                let mut all_seasons = current_seasons.clone();
                let mut added_seasons = Vec::new();

                for season in new_seasons {
                    if !all_seasons.contains(&season) {
                        all_seasons.push(season.clone());
                        added_seasons.push(season);
                    }
                }

                if !added_seasons.is_empty() {
                    // 有新季度需要添加
                    let seasons_json = serde_json::to_string(&all_seasons)?;
                    target_source.selected_seasons = Some(seasons_json);
                    target_source.download_all_seasons = Some(false); // 确保不是全部下载模式
                    updated = true;

                    merge_message = if added_seasons.len() == 1 {
                        format!("已添加新季度: {}", added_seasons.join(", "))
                    } else {
                        format!("已添加 {} 个新季度: {}", added_seasons.len(), added_seasons.join(", "))
                    };
                } else {
                    // 所有季度都已存在
                    merge_message = "所选季度已存在于目标番剧中".to_string();
                }
            }
        }
    }

    // 4. 更新保存路径（如果提供了不同的路径）
    if !params.path.is_empty() && params.path != target_source.path {
        target_source.path = params.path.clone();
        updated = true;

        if !merge_message.is_empty() {
            merge_message.push('，');
        }
        merge_message.push_str(&format!("保存路径已更新为: {}", params.path));
    }

    // 5. 更新番剧名称（如果提供了不同的名称）
    if !params.name.is_empty() && params.name != target_source.name {
        target_source.name = params.name.clone();
        updated = true;

        if !merge_message.is_empty() {
            merge_message.push('，');
        }
        merge_message.push_str(&format!("番剧名称已更新为: {}", params.name));
    }

    // 6. 更新数据库记录
    if updated {
        let mut target_update = video_source::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(target_source.id),
            latest_row_at: sea_orm::Set(crate::utils::time_format::now_standard_string()),
            ..Default::default()
        };

        if download_all_seasons {
            target_update.download_all_seasons = sea_orm::Set(Some(true));
            target_update.selected_seasons = sea_orm::Set(None);
        } else {
            // 更新特定季度选择
            if let Some(ref new_seasons_json) = target_source.selected_seasons {
                target_update.selected_seasons = sea_orm::Set(Some(new_seasons_json.clone()));
            }
            target_update.download_all_seasons = sea_orm::Set(Some(false));
        }

        if !params.path.is_empty() && params.path != target_source.path {
            target_update.path = sea_orm::Set(params.path);
        }

        if !params.name.is_empty() && params.name != target_source.name {
            target_update.name = sea_orm::Set(params.name);
        }

        video_source::Entity::update(target_update).exec(txn).await?;

        // 清除番剧缓存，强制重新扫描新合并的季度
        let clear_cache_update = video_source::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(target_source.id),
            cached_episodes: sea_orm::Set(None),
            cache_updated_at: sea_orm::Set(None),
            ..Default::default()
        };
        if let Err(e) = video_source::Entity::update(clear_cache_update).exec(txn).await {
            warn!("清除番剧缓存失败: {}", e);
        } else {
            info!("已清除番剧缓存，将在下次扫描时重新获取所有季度内容");
        }

        info!(
            "番剧已成功合并到现有源: {} (ID: {}), 变更: {}",
            target_source.name, target_source.id, merge_message
        );
    } else {
        info!(
            "番剧合并完成，无需更改: {} (ID: {})",
            target_source.name, target_source.id
        );
    }

    Ok(AddVideoSourceResponse {
        success: true,
        source_id: target_source.id,
        source_type: "bangumi".to_string(),
        message: format!("已成功合并到现有番剧源「{}」，{}", target_source.name, merge_message),
    })
}

/// 更新视频源关键词过滤器
#[utoipa::path(
    put,
    path = "/api/video-sources/{source_type}/{id}/keyword-filters",
    params(
        ("source_type" = String, Path, description = "视频源类型: collection, favorite, submission, watch_later, bangumi"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    request_body = crate::api::request::UpdateKeywordFiltersRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::UpdateKeywordFiltersResponse>),
    )
)]
pub async fn update_video_source_keyword_filters(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
    axum::Json(params): axum::Json<crate::api::request::UpdateKeywordFiltersRequest>,
) -> Result<ApiResponse<crate::api::response::UpdateKeywordFiltersResponse>, ApiError> {
    use crate::utils::keyword_filter::validate_regex;

    // 验证黑名单正则表达式
    if let Some(ref blacklist) = params.blacklist_keywords {
        for pattern in blacklist {
            if let Err(e) = validate_regex(pattern) {
                return Err(anyhow!("黑名单正则表达式验证失败: {} - {}", pattern, e).into());
            }
        }
    }

    // 验证白名单正则表达式
    if let Some(ref whitelist) = params.whitelist_keywords {
        for pattern in whitelist {
            if let Err(e) = validate_regex(pattern) {
                return Err(anyhow!("白名单正则表达式验证失败: {} - {}", pattern, e).into());
            }
        }
    }

    // 向后兼容：验证旧的关键词列表
    if let Some(ref keyword_filters) = params.keyword_filters {
        for pattern in keyword_filters {
            if let Err(e) = validate_regex(pattern) {
                return Err(anyhow!("正则表达式验证失败: {} - {}", pattern, e).into());
            }
        }
    }

    let txn = db.begin().await?;
    let mut submission_whitelist_backfill_job: Option<(submission::Model, Vec<String>, bool)> = None;

    // 处理黑名单
    let blacklist_count = params.blacklist_keywords.as_ref().map(|v| v.len()).unwrap_or(0);
    let blacklist_json = params
        .blacklist_keywords
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    // 处理白名单
    let whitelist_count = params.whitelist_keywords.as_ref().map(|v| v.len()).unwrap_or(0);
    let whitelist_json = params
        .whitelist_keywords
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    // 向后兼容：处理旧的关键词列表
    let keyword_filters_json = params
        .keyword_filters
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| serde_json::to_string(v).unwrap_or_default());
    let keyword_filter_mode = params.keyword_filter_mode.clone();

    // 处理大小写敏感设置
    let case_sensitive = params.case_sensitive.unwrap_or(true);

    let mut result = match source_type.as_str() {
        "collection" => {
            let record = collection::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            collection::Entity::update(collection::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                blacklist_keywords: sea_orm::Set(blacklist_json),
                whitelist_keywords: sea_orm::Set(whitelist_json),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode.clone()),
                keyword_case_sensitive: sea_orm::Set(case_sensitive),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateKeywordFiltersResponse {
                success: true,
                source_id: id,
                source_type: "collection".to_string(),
                blacklist_count,
                whitelist_count,
                message: format!(
                    "合集 {} 的关键词过滤器已更新，黑名单 {} 个，白名单 {} 个",
                    record.name, blacklist_count, whitelist_count
                ),
            }
        }
        "favorite" => {
            let record = favorite::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            favorite::Entity::update(favorite::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                blacklist_keywords: sea_orm::Set(blacklist_json),
                whitelist_keywords: sea_orm::Set(whitelist_json),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode.clone()),
                keyword_case_sensitive: sea_orm::Set(case_sensitive),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateKeywordFiltersResponse {
                success: true,
                source_id: id,
                source_type: "favorite".to_string(),
                blacklist_count,
                whitelist_count,
                message: format!(
                    "收藏夹 {} 的关键词过滤器已更新，黑名单 {} 个，白名单 {} 个",
                    record.name, blacklist_count, whitelist_count
                ),
            }
        }
        "submission" => {
            let record = submission::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            let whitelist_keywords_for_job = params.whitelist_keywords.clone().unwrap_or_default();
            let complex_regex_count = whitelist_keywords_for_job
                .iter()
                .map(|k| k.trim())
                .filter(|k| !k.is_empty() && !is_plain_submission_search_keyword(k))
                .count();
            let has_complex_regex = complex_regex_count > 0;
            let whitelist_changed = record.whitelist_keywords != whitelist_json;
            let is_initial_increment_cursor =
                record.latest_row_at.is_empty() || record.latest_row_at == "1970-01-01 00:00:00";
            let should_advance_cursor_after_precise_whitelist =
                whitelist_changed && !has_complex_regex && is_initial_increment_cursor;
            let filters_changed = record.blacklist_keywords != blacklist_json
                || record.whitelist_keywords != whitelist_json
                || record.keyword_filters != keyword_filters_json
                || record.keyword_filter_mode != keyword_filter_mode
                || record.keyword_case_sensitive != case_sensitive;

            let mut update_model = submission::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                blacklist_keywords: sea_orm::Set(blacklist_json),
                whitelist_keywords: sea_orm::Set(whitelist_json),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode.clone()),
                keyword_case_sensitive: sea_orm::Set(case_sensitive),
                ..Default::default()
            };

            if filters_changed {
                // 过滤规则变更后清空自适应扫描节流，让下一轮可立即执行增量扫描
                update_model.next_scan_at = sea_orm::Set(None);
                update_model.no_update_streak = sea_orm::Set(0);

                // 白名单包含复杂正则时，无法用搜索接口精准命中，必须回退到全量扫描重匹配
                if whitelist_changed && has_complex_regex {
                    update_model.latest_row_at = sea_orm::Set("1970-01-01 00:00:00".to_string());
                } else if should_advance_cursor_after_precise_whitelist {
                    // 首次扫描场景下，白名单已通过精准补抓处理历史视频，无需再走一次历史全量页
                    update_model.latest_row_at = sea_orm::Set(crate::utils::time_format::now_standard_string());
                }
            }

            submission::Entity::update(update_model).exec(&txn).await?;

            if whitelist_changed {
                submission_whitelist_backfill_job =
                    Some((record.clone(), whitelist_keywords_for_job, has_complex_regex));
            }

            let mut message = format!(
                "UP主投稿 {} 的关键词过滤器已更新，黑名单 {} 个，白名单 {} 个",
                record.upper_name, blacklist_count, whitelist_count
            );
            if whitelist_changed {
                if has_complex_regex {
                    message.push_str(&format!(
                        "；检测到复杂正则 {} 个，已触发全量扫描重匹配；其余可搜索关键词仍会精准补抓",
                        complex_regex_count
                    ));
                } else {
                    message.push_str("；将按白名单关键词精准补抓历史投稿（不触发全量扫描）");
                    if should_advance_cursor_after_precise_whitelist {
                        message.push_str("；已推进增量游标到当前时间，避免首次扫描回扫历史页");
                    }
                }
            }

            crate::api::response::UpdateKeywordFiltersResponse {
                success: true,
                source_id: id,
                source_type: "submission".to_string(),
                blacklist_count,
                whitelist_count,
                message,
            }
        }
        "watch_later" => {
            let _record = watch_later::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;

            watch_later::Entity::update(watch_later::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                blacklist_keywords: sea_orm::Set(blacklist_json),
                whitelist_keywords: sea_orm::Set(whitelist_json),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode.clone()),
                keyword_case_sensitive: sea_orm::Set(case_sensitive),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateKeywordFiltersResponse {
                success: true,
                source_id: id,
                source_type: "watch_later".to_string(),
                blacklist_count,
                whitelist_count,
                message: format!(
                    "稍后观看的关键词过滤器已更新，黑名单 {} 个，白名单 {} 个",
                    blacklist_count, whitelist_count
                ),
            }
        }
        "bangumi" => {
            let record = video_source::Entity::find_by_id(id)
                .one(&txn)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            video_source::Entity::update(video_source::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                blacklist_keywords: sea_orm::Set(blacklist_json),
                whitelist_keywords: sea_orm::Set(whitelist_json),
                keyword_filters: sea_orm::Set(keyword_filters_json),
                keyword_filter_mode: sea_orm::Set(keyword_filter_mode.clone()),
                keyword_case_sensitive: sea_orm::Set(case_sensitive),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            crate::api::response::UpdateKeywordFiltersResponse {
                success: true,
                source_id: id,
                source_type: "bangumi".to_string(),
                blacklist_count,
                whitelist_count,
                message: format!(
                    "番剧 {} 的关键词过滤器已更新，黑名单 {} 个，白名单 {} 个",
                    record.name, blacklist_count, whitelist_count
                ),
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type).into()),
    };

    txn.commit().await?;

    if let Some((submission_record, whitelist_keywords, has_complex_regex)) = submission_whitelist_backfill_job {
        if whitelist_keywords.iter().any(|k| !k.trim().is_empty()) {
            result
                .message
                .push_str("；白名单精准补抓已转为后台执行，可继续前端操作");

            let db_for_backfill = Arc::clone(&db);
            tokio::spawn(async move {
                match backfill_submission_by_whitelist_keywords(
                    db_for_backfill.as_ref(),
                    &submission_record,
                    &whitelist_keywords,
                )
                .await
                {
                    Ok(stats) => {
                        if has_complex_regex && stats.skipped_regex_keywords > 0 {
                            info!(
                                "UP主 {} 白名单精准补抓完成：复杂正则 {} 个已交由全量扫描处理",
                                submission_record.upper_name, stats.skipped_regex_keywords
                            );
                        }
                        info!(
                            "UP主 {} 白名单精准补抓完成：关键词 {} 个（可搜索 {} 个，正则/复杂模式 {} 个），命中视频 {} 个：新增入队 {} 个，恢复已删 {} 个，已存在 {} 个，非当前UP {} 个，失败 {} 个",
                            submission_record.upper_name,
                            stats.total_keywords,
                            stats.searched_keywords,
                            stats.skipped_regex_keywords,
                            stats.matched_bvids,
                            stats.backfill.queued_new,
                            stats.backfill.restored_deleted,
                            stats.backfill.already_exists,
                            stats.backfill.skipped_non_owner,
                            stats.backfill.failed
                        );
                    }
                    Err(err) => {
                        warn!("UP主 {} 白名单精准补抓失败: {}", submission_record.upper_name, err);
                        if has_complex_regex {
                            warn!(
                                "UP主 {} 白名单包含复杂正则，后续仍会通过全量扫描重匹配",
                                submission_record.upper_name
                            );
                        } else {
                            warn!(
                                "UP主 {} 白名单补抓失败，后续仅执行常规增量扫描",
                                submission_record.upper_name
                            );
                        }
                    }
                }
            });
        } else {
            result.message.push_str("；白名单已清空，不执行历史精准补抓");
        }
    }

    info!("{}", result.message);

    Ok(ApiResponse::ok(result))
}

/// 获取视频源关键词过滤器
#[utoipa::path(
    get,
    path = "/api/video-sources/{source_type}/{id}/keyword-filters",
    params(
        ("source_type" = String, Path, description = "视频源类型: collection, favorite, submission, watch_later, bangumi"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    responses(
        (status = 200, body = ApiResponse<crate::api::response::GetKeywordFiltersResponse>),
    )
)]
pub async fn get_video_source_keyword_filters(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
) -> Result<ApiResponse<crate::api::response::GetKeywordFiltersResponse>, ApiError> {
    // 定义一个辅助结构体来存储所有过滤器信息
    struct FilterInfo {
        blacklist: Vec<String>,
        whitelist: Vec<String>,
        case_sensitive: bool,
        legacy_filters: Vec<String>,
        legacy_mode: Option<String>,
    }

    let filter_info: FilterInfo = match source_type.as_str() {
        "collection" => {
            let record = collection::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            FilterInfo {
                blacklist: record
                    .blacklist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                whitelist: record
                    .whitelist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                case_sensitive: record.keyword_case_sensitive,
                legacy_filters: record
                    .keyword_filters
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                legacy_mode: record.keyword_filter_mode,
            }
        }
        "favorite" => {
            let record = favorite::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            FilterInfo {
                blacklist: record
                    .blacklist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                whitelist: record
                    .whitelist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                case_sensitive: record.keyword_case_sensitive,
                legacy_filters: record
                    .keyword_filters
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                legacy_mode: record.keyword_filter_mode,
            }
        }
        "submission" => {
            let record = submission::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            FilterInfo {
                blacklist: record
                    .blacklist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                whitelist: record
                    .whitelist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                case_sensitive: record.keyword_case_sensitive,
                legacy_filters: record
                    .keyword_filters
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                legacy_mode: record.keyword_filter_mode,
            }
        }
        "watch_later" => {
            let record = watch_later::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;

            FilterInfo {
                blacklist: record
                    .blacklist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                whitelist: record
                    .whitelist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                case_sensitive: record.keyword_case_sensitive,
                legacy_filters: record
                    .keyword_filters
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                legacy_mode: record.keyword_filter_mode,
            }
        }
        "bangumi" => {
            let record = video_source::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            FilterInfo {
                blacklist: record
                    .blacklist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                whitelist: record
                    .whitelist_keywords
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                case_sensitive: record.keyword_case_sensitive,
                legacy_filters: record
                    .keyword_filters
                    .as_ref()
                    .and_then(|json_str| serde_json::from_str(json_str).ok())
                    .unwrap_or_default(),
                legacy_mode: record.keyword_filter_mode,
            }
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type).into()),
    };

    Ok(ApiResponse::ok(crate::api::response::GetKeywordFiltersResponse {
        success: true,
        source_id: id,
        source_type,
        blacklist_keywords: filter_info.blacklist,
        whitelist_keywords: filter_info.whitelist,
        case_sensitive: filter_info.case_sensitive,
        keyword_filters: filter_info.legacy_filters,
        keyword_filter_mode: filter_info.legacy_mode,
    }))
}

/// 验证正则表达式
#[utoipa::path(
    post,
    path = "/api/validate-regex",
    request_body = crate::api::request::ValidateRegexRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::ValidateRegexResponse>),
    )
)]
pub async fn validate_regex_pattern(
    axum::Json(params): axum::Json<crate::api::request::ValidateRegexRequest>,
) -> Result<ApiResponse<crate::api::response::ValidateRegexResponse>, ApiError> {
    use crate::utils::keyword_filter::validate_regex;

    let result = match validate_regex(&params.pattern) {
        Ok(_) => crate::api::response::ValidateRegexResponse {
            valid: true,
            pattern: params.pattern,
            error: None,
        },
        Err(e) => crate::api::response::ValidateRegexResponse {
            valid: false,
            pattern: params.pattern,
            error: Some(e),
        },
    };

    Ok(ApiResponse::ok(result))
}

/// 清除AI对话历史缓存
#[utoipa::path(
    post,
    path = "/api/ai-rename/clear-cache",
    responses(
        (status = 200, body = ApiResponse<crate::api::response::ClearAiCacheResponse>),
    )
)]
pub async fn clear_ai_rename_cache() -> Result<ApiResponse<crate::api::response::ClearAiCacheResponse>, ApiError> {
    if let Err(e) = crate::utils::ai_rename::clear_all_naming_cache().await {
        return Ok(ApiResponse::ok(crate::api::response::ClearAiCacheResponse {
            success: false,
            message: format!("清除AI对话历史失败: {}", e),
        }));
    }

    Ok(ApiResponse::ok(crate::api::response::ClearAiCacheResponse {
        success: true,
        message: "AI对话历史已清除".to_string(),
    }))
}

/// 清除指定视频源的AI对话历史缓存
#[utoipa::path(
    post,
    path = "/api/ai-rename/clear-cache/{source_type}/{id}",
    params(
        ("source_type" = String, Path, description = "视频源类型"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    responses(
        (status = 200, body = ApiResponse<crate::api::response::ClearAiCacheResponse>),
    )
)]
pub async fn clear_ai_rename_cache_for_source(
    Path((source_type, id)): Path<(String, i32)>,
) -> Result<ApiResponse<crate::api::response::ClearAiCacheResponse>, ApiError> {
    let source_key = format!("{}_{}", source_type, id);
    if let Err(e) = crate::utils::ai_rename::clear_naming_cache(&source_key).await {
        return Ok(ApiResponse::ok(crate::api::response::ClearAiCacheResponse {
            success: false,
            message: format!("清除 {} 的AI对话历史失败: {}", source_key, e),
        }));
    }

    Ok(ApiResponse::ok(crate::api::response::ClearAiCacheResponse {
        success: true,
        message: format!("已清除 {} 的AI对话历史", source_key),
    }))
}

/// 批量重命名视频源下的历史文件
#[utoipa::path(
    post,
    path = "/api/{source_type}/{id}/ai-rename-history",
    params(
        ("source_type" = String, Path, description = "视频源类型 (collection/favorite/submission/watch_later/bangumi)"),
        ("id" = i32, Path, description = "视频源ID"),
    ),
    request_body = crate::api::response::BatchRenameRequest,
    responses(
        (status = 200, body = ApiResponse<crate::api::response::BatchRenameResponse>),
    )
)]
pub async fn ai_rename_history(
    Extension(db): Extension<Arc<DatabaseConnection>>,
    Path((source_type, id)): Path<(String, i32)>,
    Json(req): Json<crate::api::response::BatchRenameRequest>,
) -> Result<ApiResponse<crate::api::response::BatchRenameResponse>, ApiError> {
    use crate::task::{pause_scanning, resume_scanning};
    use crate::utils::ai_rename::{batch_rename_history_files, AiRenameConfig};

    // 扫描恢复守卫，确保函数退出时恢复扫描
    struct ScanResumeGuard {
        paused: bool,
    }
    impl Drop for ScanResumeGuard {
        fn drop(&mut self) {
            if self.paused {
                info!("AI批量重命名结束，恢复扫描任务...");
                resume_scanning();
            }
        }
    }

    // 获取全局配置
    let config = crate::config::reload_config();
    let ai_config: AiRenameConfig = config.ai_rename.clone();

    // 检查全局 AI 重命名是否启用
    if !ai_config.enabled {
        return Ok(ApiResponse::ok(crate::api::response::BatchRenameResponse {
            success: false,
            renamed_count: 0,
            skipped_count: 0,
            failed_count: 0,
            message: "AI 重命名功能未启用，请在系统设置中开启".to_string(),
        }));
    }

    // 暂停扫描任务，避免重命名过程中发生冲突
    info!("AI批量重命名开始，暂停扫描任务...");
    pause_scanning().await;
    let _guard = ScanResumeGuard { paused: true };

    // 构建 source_key
    let source_key = format!("{}_{}", source_type, id);

    // 根据 source_type 获取视频源配置和视频列表
    let (video_prompt, audio_prompt, videos, flat_folder, source_rename_parent_dir) = match source_type.as_str() {
        "collection" => {
            let source = collection::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;

            // 获取该合集下所有视频及其分页
            let videos_with_pages = get_videos_with_pages_for_source(db.as_ref(), "collection", id).await?;

            (
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                videos_with_pages,
                source.flat_folder,
                source.ai_rename_rename_parent_dir,
            )
        }
        "favorite" => {
            let source = favorite::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;

            let videos_with_pages = get_videos_with_pages_for_source(db.as_ref(), "favorite", id).await?;

            (
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                videos_with_pages,
                source.flat_folder,
                source.ai_rename_rename_parent_dir,
            )
        }
        "submission" => {
            let source = submission::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;

            let videos_with_pages = get_videos_with_pages_for_source(db.as_ref(), "submission", id).await?;

            (
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                videos_with_pages,
                source.flat_folder,
                source.ai_rename_rename_parent_dir,
            )
        }
        "watch_later" => {
            let source = watch_later::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;

            let videos_with_pages = get_videos_with_pages_for_source(db.as_ref(), "watch_later", id).await?;

            (
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                videos_with_pages,
                source.flat_folder,
                source.ai_rename_rename_parent_dir,
            )
        }
        "bangumi" => {
            let source = video_source::Entity::find_by_id(id)
                .one(db.as_ref())
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;

            let videos_with_pages = get_videos_with_pages_for_source(db.as_ref(), "bangumi", id).await?;

            (
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                videos_with_pages,
                source.flat_folder,
                source.ai_rename_rename_parent_dir,
            )
        }
        _ => {
            return Ok(ApiResponse::ok(crate::api::response::BatchRenameResponse {
                success: false,
                renamed_count: 0,
                skipped_count: 0,
                failed_count: 0,
                message: format!("不支持的视频源类型: {}", source_type),
            }));
        }
    };

    // 如果请求中提供了自定义提示词，则优先使用请求中的提示词
    let video_prompt = if !req.video_prompt.is_empty() {
        req.video_prompt.clone()
    } else {
        video_prompt
    };
    let audio_prompt = if !req.audio_prompt.is_empty() {
        req.audio_prompt.clone()
    } else {
        audio_prompt
    };

    // 如果请求中提供了高级选项，则覆盖全局配置
    let mut ai_config = ai_config;
    if let Some(enable_multi_page) = req.enable_multi_page {
        ai_config.enable_multi_page = enable_multi_page;
    }
    if let Some(enable_collection) = req.enable_collection {
        ai_config.enable_collection = enable_collection;
    }
    if let Some(enable_bangumi) = req.enable_bangumi {
        ai_config.enable_bangumi = enable_bangumi;
    }
    ai_config.rename_parent_dir = req.rename_parent_dir.unwrap_or(source_rename_parent_dir);

    if videos.is_empty() {
        return Ok(ApiResponse::ok(crate::api::response::BatchRenameResponse {
            success: true,
            renamed_count: 0,
            skipped_count: 0,
            failed_count: 0,
            message: "该视频源没有已下载的视频".to_string(),
        }));
    }

    info!("[{}] 开始批量 AI 重命名，共 {} 个视频", source_key, videos.len());

    // 记录使用的提示词（便于调试）
    if !video_prompt.is_empty() {
        info!("[{}] 视频提示词: {}", source_key, video_prompt);
    }
    if !audio_prompt.is_empty() {
        info!("[{}] 音频提示词: {}", source_key, audio_prompt);
    }

    // 执行批量重命名
    let result = batch_rename_history_files(
        db.as_ref(),
        &source_key,
        videos,
        &ai_config,
        &video_prompt,
        &audio_prompt,
        flat_folder,
    )
    .await;

    match result {
        Ok(batch_result) => Ok(ApiResponse::ok(crate::api::response::BatchRenameResponse {
            success: true,
            renamed_count: batch_result.renamed_count,
            skipped_count: batch_result.skipped_count,
            failed_count: batch_result.failed_count,
            message: format!(
                "批量重命名完成：重命名 {} 个，跳过 {} 个，失败 {} 个",
                batch_result.renamed_count, batch_result.skipped_count, batch_result.failed_count
            ),
        })),
        Err(e) => {
            error!("[{}] 批量重命名失败: {}", source_key, e);
            Ok(ApiResponse::ok(crate::api::response::BatchRenameResponse {
                success: false,
                renamed_count: 0,
                skipped_count: 0,
                failed_count: 0,
                message: format!("批量重命名失败: {}", e),
            }))
        }
    }
}

/// 获取视频源下所有已下载视频及其分页
async fn get_videos_with_pages_for_source(
    db: &DatabaseConnection,
    source_type: &str,
    source_id: i32,
) -> Result<Vec<(video::Model, Vec<page::Model>)>> {
    // 根据源类型查询视频（按发布时间正序排列，便于AI生成连续的集数编号）
    let videos = match source_type {
        "collection" => {
            video::Entity::find()
                .filter(video::Column::CollectionId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "favorite" => {
            video::Entity::find()
                .filter(video::Column::FavoriteId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "submission" => {
            video::Entity::find()
                .filter(video::Column::SubmissionId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "watch_later" => {
            video::Entity::find()
                .filter(video::Column::WatchLaterId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "bangumi" => {
            video::Entity::find()
                .filter(video::Column::SourceId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type)),
    };

    // 获取每个视频的已下载分页
    let mut result = Vec::new();
    for video_model in videos {
        // 查询已下载的分页（download_status > 0 表示至少有部分下载完成）
        let pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(video_model.id))
            .filter(page::Column::DownloadStatus.gt(0))
            .filter(page::Column::Path.is_not_null())
            .all(db)
            .await?;

        if !pages.is_empty() {
            result.push((video_model, pages));
        }
    }

    Ok(result)
}
