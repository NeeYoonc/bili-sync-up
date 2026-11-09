use core::str;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, ensure, Context, Result};
use futures::StreamExt;
use reqwest::{header, Method, StatusCode, Url};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::{debug, error, info, warn};

use crate::bilibili::Client;
pub struct Downloader {
    client: Client,
}

const BAD_CDN_HOST_TTL: Duration = Duration::from_secs(10 * 60);
const MIN_PARALLEL_SIZE: u64 = 4 * 1024 * 1024;
const MIN_SEGMENT_SIZE: u64 = 1024 * 1024;
// GoogleVideo 的长连接通常在传输数 MiB 后开始明显限速。保持较小 Range
// 分片并复用连接槽，可以继续走项目原生下载器，同时避免一个
// 50~500MiB 大分片在限速连接上持续数十分钟。
const GOOGLEVIDEO_SEGMENT_SIZE: u64 = 4 * 1024 * 1024;
/// 普通 CDN（抖音/TikTok 等）的单分片上限。
///
/// 以前非 googlevideo 的分片数就等于并发数：16 线程下 7GB 的抖音原画会被切成
/// 16 个 ~460MB 的大分片。CDN 限速/掐流时一个分片根本走不完，重试又是从分片开头
/// 重来，于是永远失败（用户看到的就是「开原画一直下载失败」）。按 16MB 切分后，
/// 一次中断只损失 16MB，而且已完成分片会记进 `.resume`，重试能接着走。
const DEFAULT_SEGMENT_SIZE: u64 = 16 * 1024 * 1024;
const RANGE_DOWNLOAD_ATTEMPTS: usize = 4;
/// 分片内断流续传：单个分片内连续多少次「零字节进展」就放弃，
/// 避免 CDN 一直掐流时在分片内部死循环（外层还有 RANGE_DOWNLOAD_ATTEMPTS 兜底）。
const RANGE_RESUME_MAX_STALLED: u32 = 3;
/// 分片内断流续传：单个分片最多发起的 Range 请求数（有进展也封顶）。
const RANGE_RESUME_MAX_REQUESTS: u32 = 8;
/// 分片内断流续传前的短暂冷却，避免刚断开就重连又被掐。
const RANGE_RESUME_BACKOFF: Duration = Duration::from_millis(500);

/// 单个文件的原生并行连接数硬上限（YouTube/GoogleVideo 单文件连接数同样受此限制）。
const MAX_PARALLEL_CONNECTIONS: usize = 32;
// 断点续传状态文件后缀（记录已完成分片，供失败后继续下载）
const RESUME_SIDECAR_SUFFIX: &str = ".resume";
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

static BAD_CDN_HOSTS: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn url_host(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

fn is_googlevideo_url(url: &str) -> bool {
    match url_host(url) {
        Some(host) if host.ends_with(".googlevideo.com") => true,
        // 单测用本地假服务器模拟 googlevideo：127.0.0.1 + `/videoplayback`。
        // 只在测试构建里生效，生产行为不受影响。
        #[cfg(test)]
        Some(host) if (host == "127.0.0.1" || host == "localhost") && url.contains("/videoplayback") => true,
        _ => false,
    }
}

/// googlevideo 音频直链（mime=audio/*，如 itag 139/140/251）在部分 CDN 节点上
/// 每个 Range 请求上限约 1MiB，超出即 403；视频直链通常允许 4MiB。用于选择
/// googlevideo 分片大小。
fn googlevideo_url_is_audio(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == "mime")
                .map(|(_, value)| value.to_ascii_lowercase())
        })
        .is_some_and(|mime| mime.starts_with("audio/"))
}

/// googlevideo 直链的 `mn` 参数携带备用 CDN 节点（如 `sn-A,sn-B`）。主节点被
/// 限速/掐断（音频 403 / TLS 中途断开）时替换 hostname 即可切到其它节点。
/// 返回「原 URL + 全部备用节点 URL」。
fn googlevideo_alternate_urls(url: &str) -> Vec<String> {
    let Ok(parsed) = Url::parse(url) else {
        return vec![url.to_string()];
    };
    let Some(host) = parsed.host_str() else {
        return vec![url.to_string()];
    };
    let host = host.to_ascii_lowercase();
    if !host.ends_with(".googlevideo.com") {
        return vec![url.to_string()];
    }
    let Some(mn) = parsed
        .query_pairs()
        .find(|(key, _)| key == "mn")
        .map(|(_, value)| value.into_owned())
    else {
        return vec![url.to_string()];
    };
    let nodes: Vec<&str> = mn.split(',').filter(|value| !value.trim().is_empty()).collect();
    if nodes.len() < 2 {
        return vec![url.to_string()];
    }
    // hostname 形如 rr5---sn-oguesndz.googlevideo.com；保持 rr 前缀并替换 sn- 段
    let base = host.strip_suffix(".googlevideo.com").unwrap_or(&host);
    let current_sn = base.rsplit("---").next().unwrap_or("");
    let prefix = base.strip_suffix(current_sn).unwrap_or("");
    let mut candidates = Vec::with_capacity(nodes.len());
    candidates.push(url.to_string());
    for node in nodes {
        let node = node.trim();
        if node.is_empty() || node == current_sn {
            continue;
        }
        let sn = node.trim_start_matches("sn-");
        if sn.is_empty() {
            continue;
        }
        let mut alt = parsed.clone();
        if alt.set_host(Some(&format!("{prefix}sn-{sn}.googlevideo.com"))).is_ok() {
            candidates.push(alt.to_string());
        }
    }
    candidates
}

fn prune_expired_bad_cdn_hosts(cache: &mut HashMap<String, Instant>) {
    let now = Instant::now();
    cache.retain(|_, marked_at| now.duration_since(*marked_at) <= BAD_CDN_HOST_TTL);
}

pub(crate) fn is_url_blocked_by_bad_cdn_host(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };

    let mut cache = BAD_CDN_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
    prune_expired_bad_cdn_hosts(&mut cache);
    cache.contains_key(&host)
}

fn mark_bad_cdn_host(url: &str, err: &anyhow::Error) {
    let Some(host) = url_host(url) else {
        return;
    };

    let mut cache = BAD_CDN_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
    prune_expired_bad_cdn_hosts(&mut cache);
    let is_new = cache.insert(host.clone(), Instant::now()).is_none();
    if is_new {
        warn!(
            "检测到 CDN 证书域名不匹配，{} 分钟内跳过该 host: {}，错误: {:#}",
            BAD_CDN_HOST_TTL.as_secs() / 60,
            host,
            err
        );
    } else {
        debug!("刷新坏 CDN host 缓存: {}", host);
    }
}

fn contains_certificate_name_mismatch(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    (message.contains("invalid peer certificate") && message.contains("certificate not valid for name"))
        || message.contains("remotecertificatenamemismatch")
        || message.contains("sec_e_wrong_principal")
}

fn contains_tls_close_notify_eof(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("peer closed connection without sending tls close_notify") || message.contains("unexpected eof")
}

fn is_expected_single_connection_fallback(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}");
    message.contains("文件过小")
        || message.contains("无法获取文件大小")
        || message.contains("服务器不支持Range分片下载")
        || message.contains("分片数不足")
}

pub(crate) fn is_certificate_name_mismatch_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| contains_certificate_name_mismatch(&cause.to_string()))
        || contains_certificate_name_mismatch(&format!("{:#}", err))
}

pub(crate) fn should_refresh_playurl_after_download_error(err: &anyhow::Error) -> bool {
    let message = format!("{:#}", err);
    message.contains("所有URL尝试失败") || message.contains("failed to download from")
}

fn media_tool_executable_name(tool: &str) -> String {
    #[cfg(windows)]
    {
        return match tool {
            "ffmpeg" => "ffmpeg.exe".to_string(),
            "ffprobe" => "ffprobe.exe".to_string(),
            _ => tool.to_string(),
        };
    }

    #[cfg(not(windows))]
    {
        tool.to_string()
    }
}

#[cfg(windows)]
fn normalize_windows_exe_path(path: &Path) -> PathBuf {
    if path.extension().is_none() {
        let exe_path = path.with_extension("exe");
        if exe_path.exists() {
            return exe_path;
        }
    }
    path.to_path_buf()
}

#[cfg(not(windows))]
fn normalize_windows_exe_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// 解析媒体工具可执行路径：
/// - 优先使用配置中的 `ffmpeg_path`（可填目录或 ffmpeg 可执行文件路径）
/// - 若未配置或解析失败，则回退到系统 PATH（ffmpeg/ffprobe）
pub fn resolve_media_tool_path(tool: &str) -> PathBuf {
    let fallback = PathBuf::from(media_tool_executable_name(tool));
    let configured_path = crate::config::with_config(|bundle| bundle.config.ffmpeg_path.clone());
    let configured_path = configured_path.trim();

    if configured_path.is_empty() {
        return fallback;
    }

    let configured = PathBuf::from(configured_path);
    if configured.is_dir() {
        let candidate = normalize_windows_exe_path(&configured.join(media_tool_executable_name(tool)));
        return if candidate.exists() { candidate } else { fallback };
    }

    let configured = normalize_windows_exe_path(&configured);
    if tool.eq_ignore_ascii_case("ffmpeg") {
        return if configured.exists() { configured } else { fallback };
    }

    if tool.eq_ignore_ascii_case("ffprobe") {
        if let Some(parent) = configured.parent() {
            let sibling = normalize_windows_exe_path(&parent.join(media_tool_executable_name("ffprobe")));
            if sibling.exists() {
                return sibling;
            }
        }
    }

    fallback
}

impl Downloader {
    // Downloader 使用带有默认 Header 的 Client 构建
    // 拿到 url 后下载文件不需要任何 cookie 作为身份凭证
    // 但如果不设置默认 Header，下载时会遇到 403 Forbidden 错误
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn fetch(&self, url: &str, path: &Path) -> Result<()> {
        self.fetch_with_optional_referer(url, path, None).await
    }

    pub async fn fetch_with_referer(&self, url: &str, path: &Path, referer: &str) -> Result<()> {
        self.fetch_with_optional_referer(url, path, Some(referer)).await
    }

    async fn fetch_with_optional_referer(&self, url: &str, path: &Path, referer: Option<&str>) -> Result<()> {
        let config = crate::config::reload_config();
        let parallel = &config.concurrent_limit.parallel_download;

        let is_googlevideo = is_googlevideo_url(url);
        let file_concurrency =
            parallel_file_concurrency(parallel.threads, parallel.googlevideo_max_connections, is_googlevideo);
        if parallel.enabled && file_concurrency > 1 {
            match self
                .fetch_parallel(
                    url,
                    path,
                    parallel.threads,
                    parallel.googlevideo_max_connections,
                    referer,
                    None,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) if is_certificate_name_mismatch_error(&e) => return Err(e),
                Err(e) => {
                    let host = url_host(url).unwrap_or_else(|| "unknown".to_string());
                    if is_expected_single_connection_fallback(&e) {
                        debug!(
                            host,
                            path = %path.display(),
                            reason = %format!("{e:#}"),
                            "资源「{}」无需或无法进行 Range 分片，改用原生单连接下载",
                            path.display()
                        );
                    } else {
                        debug!(
                            host,
                            path = %path.display(),
                            error = %format!("{e:#}"),
                            "资源「{}」原生多线程分片下载失败，改用断点续传顺序补下",
                            path.display()
                        );
                    }
                }
            }
        }

        // googlevideo 直链拒绝无 Range 的完整 GET（403/掐断连接），必须走有界 Range。
        // 失败时保留已下载数据与 `.resume` 分片状态，下次（含进程重启/直链刷新后）
        // 继续续传，而不是删掉半成品从头下载。
        if is_googlevideo_url(url) {
            self.fetch_googlevideo_single(url, path, referer).await
        } else {
            // 断点续传：并发分片失败后按顺序补下未完成分片；
            // 失败时保留已下载数据与分片状态，下次继续而不是从头下载。
            self.fetch_range_resume(url, path, referer, None).await
        }
    }

    /// googlevideo 直链拒绝无 Range 的完整 GET（403/掐断连接），多线程分片又因
    /// 探测失败不可用时的兜底：重新探测总大小，再按有界 Range（音频 1MiB、
    /// 视频 4MiB）顺序下载。已完成分片同样记录在 `.resume` 中，可接着续传。
    async fn fetch_googlevideo_single(&self, url: &str, path: &Path, referer: Option<&str>) -> Result<()> {
        let (range_supported, probe_size) = self.probe_range_support_and_size(url, referer, None).await?;
        let total_size = probe_size
            .filter(|size| *size > 0)
            .ok_or_else(|| anyhow!("无法获取 googlevideo 文件大小（Range 探测失败）"))?;
        ensure!(range_supported, "googlevideo 未返回 Range 支持");
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }
        let segment = if googlevideo_url_is_audio(url) {
            MIN_SEGMENT_SIZE
        } else {
            GOOGLEVIDEO_SEGMENT_SIZE
        };
        let (_, ranges) = build_parallel_ranges(total_size, 1, true, segment, 1);

        let sidecar = resume_sidecar_path(path);
        let existing_len = tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
        let completed: HashSet<(u64, u64)> = if existing_len == total_size {
            load_completed_ranges(&sidecar, url).await
        } else {
            remove_sidecar(&sidecar).await;
            HashSet::new()
        };
        if existing_len != total_size {
            let file = File::create(path).await?;
            file.set_len(total_size).await?;
        }
        let pending_ranges: Vec<(u64, u64)> = ranges
            .iter()
            .copied()
            .filter(|range| !completed.contains(range))
            .collect();
        if pending_ranges.is_empty() {
            if !is_media_stream_tmp_path(path) {
                remove_sidecar(&sidecar).await;
            }
            return Ok(());
        }
        ensure_resume_fingerprint(&sidecar, url).await;
        let completed_bytes: u64 = ranges
            .iter()
            .filter(|range| completed.contains(range))
            .map(|(start, end)| end.saturating_sub(*start) + 1)
            .sum();
        let mut downloaded = 0u64;
        for (start, end) in pending_ranges {
            let bytes = download_range_to_file_with_retry(DownloadRangeInput {
                client: self.client.clone(),
                url,
                path,
                start,
                end,
                attempts: RANGE_DOWNLOAD_ATTEMPTS,
                referer,
                cookie: None,
            })
            .await?;
            append_completed_range(&sidecar, start, end).await;
            downloaded = downloaded.saturating_add(bytes);
        }
        ensure!(
            downloaded + completed_bytes == total_size,
            "分片下载大小不一致: {} + {} != {}",
            downloaded,
            completed_bytes,
            total_size
        );
        if !is_media_stream_tmp_path(path) {
            remove_sidecar(&sidecar).await;
        }
        Ok(())
    }

    /// 断点续传兜底：并发分片失败后，按顺序补下未完成分片。
    /// 已完成分片记录在 `.resume` 状态文件中；若本次仍失败，保留部分数据与
    /// 状态，下次下载（包括上层刷新直链后的重试）会继续而不是从头开始。
    async fn fetch_range_resume(
        &self,
        url: &str,
        path: &Path,
        referer: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<()> {
        let (total_size, range_supported) = self.get_size_and_range_support(url, referer, cookie).await?;
        crate::download_progress::DOWNLOAD_PROGRESS
            .set_stream_total(&path.display().to_string(), total_size)
            .await;
        if total_size == 0 || !range_supported {
            // 不支持 Range 分片：退回全量单连接下载
            return self.fetch_single(url, path, referer, cookie).await;
        }
        // 与并发分片使用相同的分片方式，保证断点状态中的分片能精确命中
        let config = crate::config::reload_config();
        let threads = config.concurrent_limit.parallel_download.threads.max(1);
        let max_googlevideo_connections = config.concurrent_limit.parallel_download.googlevideo_max_connections;
        let (_, ranges) = build_parallel_ranges(total_size, threads, false, 0, max_googlevideo_connections);
        if ranges.is_empty() {
            return self.fetch_single(url, path, referer, cookie).await;
        }

        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }

        let sidecar = resume_sidecar_path(path);
        let existing_len = tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
        let completed: HashSet<(u64, u64)> = if existing_len == total_size {
            load_completed_ranges(&sidecar, url).await
        } else {
            // 文件缺失或大小不一致：丢弃旧状态，重新全量下载
            remove_sidecar(&sidecar).await;
            HashSet::new()
        };
        if existing_len != total_size {
            let file = File::create(path).await?;
            file.set_len(total_size).await?;
        }

        let pending: Vec<(u64, u64)> = ranges
            .iter()
            .copied()
            .filter(|range| !completed.contains(range))
            .collect();
        if !pending.is_empty() {
            ensure_resume_fingerprint(&sidecar, url).await;
        }

        // 进度：续传已完成的字节作为基数，顺序补下时逐片累加
        let progress_base: u64 = ranges
            .iter()
            .filter(|range| completed.contains(range))
            .map(|(start, end)| end.saturating_sub(*start) + 1)
            .sum();
        let mut progress_done = progress_base;
        let progress_path = path.display().to_string();
        for (start, end) in pending {
            download_range_to_file_with_retry(DownloadRangeInput {
                client: self.client.clone(),
                url,
                path,
                start,
                end,
                attempts: RANGE_DOWNLOAD_ATTEMPTS,
                referer,
                cookie,
            })
            .await?;
            append_completed_range(&sidecar, start, end).await;
            progress_done = progress_done.saturating_add(end.saturating_sub(start) + 1);
            crate::download_progress::DOWNLOAD_PROGRESS
                .report_bytes(&progress_path, progress_done)
                .await;
        }

        let size = tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
        ensure!(
            size == total_size,
            "断点续传后文件大小不一致: {} != {}",
            size,
            total_size
        );

        // 封面/头像等普通资源成功后清理断点状态，避免残留 `.resume` 文件
        if !is_media_stream_tmp_path(path) {
            remove_sidecar(&sidecar).await;
        }

        Ok(())
    }

    async fn fetch_single(&self, url: &str, path: &Path, referer: Option<&str>, cookie: Option<&str>) -> Result<()> {
        // 创建父目录
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }

        let mut file = match File::create(path).await {
            Ok(f) => f,
            Err(e) => {
                error!("创建文件失败: {:#}", e);
                return Err(e.into());
            }
        };

        let request = self.client.media_request(Method::GET, url);
        let request = if let Some(referer) = referer {
            request.header(header::REFERER, referer)
        } else {
            request
        };
        let request = if let Some(cookie) = cookie {
            request.header(header::COOKIE, cookie)
        } else {
            request
        };
        let resp = match tokio::time::timeout(FIRST_BYTE_TIMEOUT, request.send()).await {
            Err(_) => return Err(anyhow!("建立下载响应超时（{} 秒）", FIRST_BYTE_TIMEOUT.as_secs())),
            Ok(result) => match result {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => r,
                    Err(e) => {
                        // 单次尝试的失败由上层统一分类：能刷新直链重试的按 debug 记录，
                        // 真正无法恢复时上层才会写 warn/error，避免自愈事件刷屏。
                        debug!("HTTP状态码错误: {:#}", e);
                        return Err(e.into());
                    }
                },
                Err(e) => {
                    debug!("HTTP请求失败: {:#}", e);
                    return Err(e.into());
                }
            },
        };

        let expected = resp.header_content_length().unwrap_or_default();

        let mut received = 0u64;
        let mut stream = resp.bytes_stream();
        let mut first_chunk = true;
        loop {
            let wait = if first_chunk {
                FIRST_BYTE_TIMEOUT
            } else {
                CHUNK_IDLE_TIMEOUT
            };
            let next = tokio::time::timeout(wait, stream.next()).await.map_err(|_| {
                if first_chunk {
                    anyhow!("等待下载首字节超时（{} 秒）", wait.as_secs())
                } else {
                    anyhow!("下载数据块空闲超时（{} 秒）", wait.as_secs())
                }
            })?;
            let Some(chunk) = next else { break };
            first_chunk = false;
            match chunk {
                Ok(chunk) => {
                    file.write_all(&chunk).await?;
                    received += chunk.len() as u64;
                }
                Err(error)
                    if expected > 0 && received >= expected && contains_tls_close_notify_eof(&error.to_string()) =>
                {
                    // 部分 GoogleVideo CDN 在完整发送 Content-Length 后直接断开 TLS，
                    // 不发送 close_notify。字节数完整时应视为成功，而不是反复重下。
                    warn!(
                        "CDN 未发送 TLS close_notify，但文件已完整接收: received={} expected={}",
                        received, expected
                    );
                    break;
                }
                Err(error) => {
                    debug!("下载过程中出错: {:#}", error);
                    return Err(error.into());
                }
            }
        }

        file.flush().await?;

        ensure!(received > 0, "下载完成但未收到任何数据");
        ensure!(
            received >= expected,
            "received {} bytes, expected {} bytes",
            received,
            expected
        );

        Ok(())
    }

    async fn fetch_parallel(
        &self,
        url: &str,
        path: &Path,
        threads: usize,
        max_googlevideo_connections: usize,
        referer: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<()> {
        let is_googlevideo = is_googlevideo_url(url);
        self.fetch_parallel_impl(
            url,
            path,
            threads,
            max_googlevideo_connections,
            referer,
            cookie,
            is_googlevideo,
        )
        .await
    }

    /// `fetch_parallel` 的实际实现。`is_googlevideo` 由调用方注入，测试可以
    /// 直接传 `true` 覆盖 googlevideo 专属分支（无需真实 googlevideo 域名）。
    // 参数与 `fetch_parallel` 保持一致，额外多一个可注入的 `is_googlevideo`；
    // 拆成 impl 是为了让单测覆盖 googlevideo 专属分支（CI 无法造真实域名）。
    #[allow(clippy::too_many_arguments)]
    async fn fetch_parallel_impl(
        &self,
        url: &str,
        path: &Path,
        threads: usize,
        max_googlevideo_connections: usize,
        referer: Option<&str>,
        cookie: Option<&str>,
        is_googlevideo: bool,
    ) -> Result<()> {
        // 小型 GoogleVideo 单连接也可能在完整传输前直接关闭 TLS；从 1MiB 起走
        // Range 分片可让失败只影响一个小分片。音频直链在部分节点上每个 Range
        // 上限仅约 1MiB（超出 403），音频统一使用 1MiB 分片。
        let min_parallel_size = if is_googlevideo {
            MIN_SEGMENT_SIZE
        } else {
            MIN_PARALLEL_SIZE
        };
        let googlevideo_segment = if is_googlevideo && googlevideo_url_is_audio(url) {
            MIN_SEGMENT_SIZE
        } else {
            GOOGLEVIDEO_SEGMENT_SIZE
        };

        // 创建父目录
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }

        let (total_size, range_supported) = self.get_size_and_range_support(url, referer, cookie).await?;
        crate::download_progress::DOWNLOAD_PROGRESS
            .set_stream_total(&path.display().to_string(), total_size)
            .await;
        ensure!(total_size > 0, "无法获取文件大小");
        ensure!(
            total_size >= min_parallel_size,
            "文件过小({} bytes)，不启用分片下载",
            total_size
        );
        ensure!(range_supported, "服务器不支持Range分片下载");

        let (concurrency, ranges) = build_parallel_ranges(
            total_size,
            threads,
            is_googlevideo,
            googlevideo_segment,
            max_googlevideo_connections,
        );
        if ranges.len() <= 1 {
            // googlevideo 小文件（短音频常见，如 2~3MB 的 m4a）只有一个分片时，
            // 不能跳过多线程后回退到无 Range 的完整 GET——googlevideo 对无 Range
            // 请求直接 403/掐断连接。改用有界 Range 单连接下载整个文件。
            if let Some(parent) = path.parent() {
                if !parent.exists() {
                    fs::create_dir_all(parent).await?;
                }
            }
            let file = File::create(path).await?;
            file.set_len(total_size).await?;
            drop(file);
            let (part_start, part_end) = ranges[0];
            download_range_to_file_with_retry(DownloadRangeInput {
                client: self.client.clone(),
                url,
                path,
                start: part_start,
                end: part_end,
                attempts: RANGE_DOWNLOAD_ATTEMPTS,
                referer,
                cookie,
            })
            .await?;
            crate::download_progress::DOWNLOAD_PROGRESS
                .report_bytes(&path.display().to_string(), total_size)
                .await;
            return Ok(());
        }

        let total_mb = total_size as f64 / 1024.0 / 1024.0;

        // 断点续传：读取已完成分片状态，仅下载未完成分片。
        // googlevideo 也启用：内容指纹取自 query 中的 id/itag/mh/mn，直链刷新
        // （expire/sig/n 变化）后指纹不变，可跨进程/跨 CDN 节点继续下载。
        let sidecar = resume_sidecar_path(path);
        let existing_len = tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
        let completed: HashSet<(u64, u64)> = if existing_len == total_size {
            load_completed_ranges(&sidecar, url).await
        } else {
            // 文件缺失或大小不一致：丢弃旧状态，重新全量下载
            remove_sidecar(&sidecar).await;
            HashSet::new()
        };
        let pending_ranges: Vec<(u64, u64)> = ranges
            .iter()
            .copied()
            .filter(|range| !completed.contains(range))
            .collect();

        if existing_len != total_size {
            // 预创建并设置目标文件大小，便于随机写入
            let file = File::create(path).await?;
            file.set_len(total_size).await?;
        }

        if pending_ranges.is_empty() {
            // 所有分片此前已全部完成（断点续传命中）
            if is_media_stream_tmp_path(path) {
                // 音视频流：保留状态文件，若后续音频下载失败并重试，可再次快速跳过已全部分片
            } else {
                remove_sidecar(&sidecar).await;
            }
            return Ok(());
        }

        if !completed.is_empty() {
            info!(
                "断点续传: 文件「{}」已存在 {} 个完成分片，继续下载剩余 {} 个分片",
                path.display(),
                completed.len(),
                pending_ranges.len()
            );
        }

        info!(
            "原生多线程下载启用: 文件「{}」, 大小={:.2}MB, 分片数={}, 并发连接={}",
            path.display(),
            total_mb,
            ranges.len(),
            concurrency
        );

        ensure_resume_fingerprint(&sidecar, url).await;
        let completed_bytes: u64 = ranges
            .iter()
            .filter(|range| completed.contains(range))
            .map(|(start, end)| end.saturating_sub(*start) + 1)
            .sum();
        // 进度：并发分片累计已下载字节（含续传已完成部分）
        let downloaded_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(completed_bytes));
        let progress_path = path.display().to_string();

        let url_owned = url.to_string();
        let path_owned = path.to_path_buf();
        let sidecar_owned = sidecar.clone();
        let referer_owned = referer.map(str::to_string);
        let cookie_owned = cookie.map(str::to_string);
        let tasks = futures::stream::iter(pending_ranges.into_iter().map(|(part_start, part_end)| {
            let client = self.client.clone();
            let url = url_owned.clone();
            let path = path_owned.clone();
            let sidecar = sidecar_owned.clone();
            let referer = referer_owned.clone();
            let cookie = cookie_owned.clone();
            let downloaded_counter = downloaded_counter.clone();
            let progress_path = progress_path.clone();
            async move {
                let result = download_range_to_file_with_retry(DownloadRangeInput {
                    client,
                    url: &url,
                    path: &path,
                    start: part_start,
                    end: part_end,
                    attempts: RANGE_DOWNLOAD_ATTEMPTS,
                    referer: referer.as_deref(),
                    cookie: cookie.as_deref(),
                })
                .await;
                if result.is_ok() {
                    // 记录已完成分片：失败时保留状态，下次可断点续传
                    append_completed_range(&sidecar, part_start, part_end).await;
                    // 进度：分片完成后累加已下载字节
                    let len = part_end.saturating_sub(part_start) + 1;
                    let acc = downloaded_counter.fetch_add(len, std::sync::atomic::Ordering::Relaxed) + len;
                    crate::download_progress::DOWNLOAD_PROGRESS
                        .report_bytes(&progress_path, acc)
                        .await;
                }
                ((part_start, part_end), result)
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
        let mut downloaded = 0u64;
        let mut failed_ranges: Vec<(u64, u64)> = Vec::new();
        for ((start, end), result) in tasks {
            match result {
                Ok(bytes) => downloaded = downloaded.saturating_add(bytes),
                Err(error) => {
                    debug!(
                        start,
                        end,
                        error = %error,
                        "Range 分片并发阶段重试后仍失败，改为串行补下"
                    );
                    failed_ranges.push((start, end));
                }
            }
        }
        if !failed_ranges.is_empty() {
            // 第二轮：失败分片改成串行 + 冷却后再补。并发挤兑（CDN 限速）下这样
            // 通常能过；已经下完的分片不会白下，也不用整体重来。
            tokio::time::sleep(Duration::from_secs(3)).await;
            for (start, end) in failed_ranges {
                let bytes = download_range_to_file_with_retry(DownloadRangeInput {
                    client: self.client.clone(),
                    url,
                    path,
                    start,
                    end,
                    attempts: RANGE_DOWNLOAD_ATTEMPTS,
                    referer,
                    cookie,
                })
                .await
                .with_context(|| format!("Range 分片补下仍失败: bytes={start}-{end}"))?;
                append_completed_range(&sidecar, start, end).await;
                downloaded = downloaded.saturating_add(bytes);
            }
        }
        ensure!(
            downloaded + completed_bytes == total_size,
            "分片下载大小不一致: {} + {} != {}",
            downloaded,
            completed_bytes,
            total_size
        );

        // 封面/头像等普通资源不留断点状态；
        // 只有音视频流临时文件保留状态，供上层合并失败/重试时续传
        if !is_media_stream_tmp_path(path) {
            remove_sidecar(&sidecar).await;
        }

        Ok(())
    }

    async fn get_size_and_range_support(
        &self,
        url: &str,
        referer: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<(u64, bool)> {
        let mut total_size = None;
        let mut range_supported = false;

        let request = self
            .client
            .media_request(Method::HEAD, url)
            .header(header::ACCEPT_ENCODING, "identity");
        let request = if let Some(referer) = referer {
            request.header(header::REFERER, referer)
        } else {
            request
        };
        let request = if let Some(cookie) = cookie {
            request.header(header::COOKIE, cookie)
        } else {
            request
        };
        let head_resp = tokio::time::timeout(FIRST_BYTE_TIMEOUT, request.send())
            .await
            .ok()
            .and_then(Result::ok);

        if let Some(resp) = head_resp {
            if let Ok(resp) = resp.error_for_status() {
                total_size = resp.header_content_length().filter(|size| *size > 0);

                let accept_ranges = resp
                    .headers()
                    .get(header::ACCEPT_RANGES)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                range_supported = accept_ranges.to_ascii_lowercase().contains("bytes");
            }
        }

        if !range_supported || total_size.is_none() {
            let (probe_supported, probe_size) = self.probe_range_support_and_size(url, referer, cookie).await?;
            range_supported = range_supported || probe_supported;
            if total_size.is_none() {
                total_size = probe_size.filter(|size| *size > 0);
            }
        }

        Ok((total_size.unwrap_or(0), range_supported))
    }

    async fn probe_range_support_and_size(
        &self,
        url: &str,
        referer: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<(bool, Option<u64>)> {
        let request = self
            .client
            .media_request(Method::GET, url)
            .header(header::RANGE, "bytes=0-0")
            .header(header::ACCEPT_ENCODING, "identity");
        let request = if let Some(referer) = referer {
            request.header(header::REFERER, referer)
        } else {
            request
        };
        let request = if let Some(cookie) = cookie {
            request.header(header::COOKIE, cookie)
        } else {
            request
        };
        let resp = tokio::time::timeout(FIRST_BYTE_TIMEOUT, request.send())
            .await
            .map_err(|_| anyhow!("Range探测响应超时（{} 秒）", FIRST_BYTE_TIMEOUT.as_secs()))?
            .context("Range探测请求失败")?;

        let status = resp.status();
        if status == StatusCode::PARTIAL_CONTENT {
            let total_size = resp.header_file_size();
            Ok((true, total_size))
        } else {
            Ok((false, None))
        }
    }

    pub async fn fetch_with_fallback(&self, urls: &[&str], path: &Path) -> Result<()> {
        self.fetch_with_fallback_and_optional_referer(urls, path, None).await
    }

    /// 使用项目原生下载逻辑下载文件，但只为本次任务的媒体请求应用代理。
    pub async fn fetch_with_fallback_with_proxy(&self, urls: &[&str], path: &Path, proxy: &str) -> Result<()> {
        let proxy = proxy.trim();
        if proxy.is_empty() {
            return self.fetch_with_fallback(urls, path).await;
        }
        let downloader = Self::new(self.client.with_media_proxy(proxy)?);
        downloader.fetch_with_fallback(urls, path).await
    }

    pub async fn fetch_with_fallback_with_referer(&self, urls: &[&str], path: &Path, referer: &str) -> Result<()> {
        self.fetch_with_fallback_and_optional_referer(urls, path, Some(referer))
            .await
    }

    /// 下载需要网页会话 Cookie 的平台媒体。Cookie 会传入 HEAD、Range
    /// 探测和每个分片，因此与项目其他媒体一样遵循全局原生多线程设置。
    pub async fn fetch_with_fallback_with_referer_and_cookie(
        &self,
        urls: &[&str],
        path: &Path,
        referer: &str,
        cookie: &str,
    ) -> Result<()> {
        if cookie.trim().is_empty() {
            return self.fetch_with_fallback_with_referer(urls, path, referer).await;
        }
        if urls.is_empty() {
            bail!("no urls provided");
        }
        let config = crate::config::reload_config();
        let parallel = &config.concurrent_limit.parallel_download;
        let mut last_error = None;
        for url in urls {
            let is_googlevideo = is_googlevideo_url(url);
            let file_concurrency =
                parallel_file_concurrency(parallel.threads, parallel.googlevideo_max_connections, is_googlevideo);
            let result = if parallel.enabled && file_concurrency > 1 {
                match self
                    .fetch_parallel(
                        url,
                        path,
                        parallel.threads,
                        parallel.googlevideo_max_connections,
                        Some(referer),
                        Some(cookie),
                    )
                    .await
                {
                    Ok(()) => Ok(()),
                    Err(error) if is_certificate_name_mismatch_error(&error) => Err(error),
                    Err(error) => {
                        let host = url_host(url).unwrap_or_else(|| "unknown".to_string());
                        debug!(
                            host,
                            path = %path.display(),
                            reason = %format!("{error:#}"),
                            "Cookie 媒体「{}」并发分片失败，改用断点续传顺序补下",
                            path.display()
                        );
                        self.fetch_range_resume(url, path, Some(referer), Some(cookie)).await
                    }
                }
            } else {
                self.fetch_single(url, path, Some(referer), Some(cookie)).await
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    // 保留部分下载数据与断点状态，下次重试可续传而不是从头下载；
                    // 调用方会按错误类型决定记录级别（可自愈的走 debug）。
                    debug!("下载资源「{}」失败: {error:#}", path.display());
                    last_error = Some(error);
                }
            }
        }
        match last_error {
            Some(error) => Err(error)
                .context("所有URL尝试失败")
                .with_context(|| format!("failed to download from {:?}", urls)),
            None => bail!("所有URL尝试失败"),
        }
    }

    async fn fetch_with_fallback_and_optional_referer(
        &self,
        urls: &[&str],
        path: &Path,
        referer: Option<&str>,
    ) -> Result<()> {
        if urls.is_empty() {
            bail!("no urls provided");
        }

        let mut last_error = None;
        for url in urls.iter() {
            // googlevideo 直链主节点被限速/掐断时，逐个尝试 `mn` 参数里的备用
            // CDN 节点（替换 hostname），避免整轮失败。
            for candidate in googlevideo_alternate_urls(url) {
                if is_url_blocked_by_bad_cdn_host(&candidate) {
                    debug!("跳过短期内已判定证书异常的 CDN URL: {}", candidate);
                    continue;
                }

                let result = match referer {
                    Some(referer) => self.fetch_with_referer(&candidate, path, referer).await,
                    None => self.fetch(&candidate, path).await,
                };
                match result {
                    Ok(_) => {
                        return Ok(());
                    }
                    Err(err) => {
                        if is_certificate_name_mismatch_error(&err) {
                            mark_bad_cdn_host(&candidate, &err);
                        }
                        debug!("下载资源「{}」失败: {:#}", path.display(), err);
                        last_error = Some(err);
                    }
                }
            }
        }

        warn!("资源「{}」的所有 URL 尝试失败", path.display());
        match last_error {
            Some(err) => Err(err)
                .context("所有URL尝试失败")
                .with_context(|| format!("failed to download from {:?}", urls)),
            None => Err(anyhow!("所有URL尝试失败：候选URL已被短期坏CDN缓存跳过"))
                .with_context(|| format!("failed to download from {:?}", urls)),
        }
    }

    pub async fn merge(&self, video_path: &Path, audio_path: &Path, output_path: &Path) -> Result<()> {
        // 检查输入文件是否存在
        if !video_path.exists() {
            error!("视频文件不存在: {}", video_path.display());
            bail!("视频文件不存在: {}", video_path.display());
        }

        if !audio_path.exists() {
            error!("音频文件不存在: {}", audio_path.display());
            bail!("音频文件不存在: {}", audio_path.display());
        }

        // 增强的文件完整性检查
        if let Err(e) = self.validate_media_file(video_path, "视频").await {
            error!("视频文件完整性检查失败: {:#}", e);
            bail!("视频文件损坏或不完整: {}", e);
        }

        if let Err(e) = self.validate_media_file(audio_path, "音频").await {
            error!("音频文件完整性检查失败: {:#}", e);
            bail!("音频文件损坏或不完整: {}", e);
        }

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }

        // 将Path转换为字符串，防止临时值过早释放
        let video_path_str = video_path.to_string_lossy().to_string();
        let audio_path_str = audio_path.to_string_lossy().to_string();
        let output_path_str = output_path.to_string_lossy().to_string();

        // 构建FFmpeg命令
        let args = [
            "-i",
            &video_path_str,
            "-i",
            &audio_path_str,
            "-c",
            "copy",
            "-strict",
            "unofficial",
            "-y",
            &output_path_str,
        ];

        let output = crate::utils::process::tokio_command(resolve_media_tool_path("ffmpeg"))
            .args(args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
            error!("FFmpeg错误: {}", stderr);
            bail!("ffmpeg error: {}", stderr);
        }

        // `-c copy` 正常只增加少量封装开销；如果输出明显小于视频输入，
        // 说明输入尾部没有被完整写入或 muxer 中途丢弃了数据。不能让这种文件
        // 继续以「下载完成」落库（Issue #254 的 1.7GB vs 2GB 就是这种表现）。
        let video_size = tokio::fs::metadata(video_path)
            .await
            .with_context(|| format!("无法读取合并前视频大小: {}", video_path.display()))?
            .len();
        let audio_size = tokio::fs::metadata(audio_path)
            .await
            .with_context(|| format!("无法读取合并前音频大小: {}", audio_path.display()))?
            .len();
        let output_size = tokio::fs::metadata(output_path)
            .await
            .with_context(|| format!("无法读取合并后文件大小: {}", output_path.display()))?
            .len();
        info!(
            video_path = %video_path.display(),
            video_size,
            audio_size,
            output_size,
            "音视频合并完成，开始校验输出大小"
        );
        ensure!(
            merged_output_size_is_complete(video_size, output_size),
            "合并后文件异常偏小: 输出 {} bytes，视频流 {} bytes，音频流 {} bytes；可能是输入尾部缺失或 FFmpeg 只写入了部分数据",
            output_size,
            video_size,
            audio_size
        );

        Ok(())
    }

    /// 验证媒体文件的完整性
    async fn validate_media_file(&self, file_path: &Path, file_type: &str) -> Result<()> {
        // 检查文件大小
        let metadata = tokio::fs::metadata(file_path)
            .await
            .with_context(|| format!("无法读取{}文件元数据: {}", file_type, file_path.display()))?;

        let file_size = metadata.len();
        if file_size == 0 {
            bail!("{}文件为空: {}", file_type, file_path.display());
        }

        if file_size < 1024 {
            // 小于1KB很可能是损坏的
            bail!(
                "{}文件过小({}字节)，可能损坏: {}",
                file_type,
                file_size,
                file_path.display()
            );
        }

        // 使用ffprobe快速验证文件格式
        let file_path_str = file_path.to_string_lossy().to_string();
        let result = crate::utils::process::tokio_command(resolve_media_tool_path("ffprobe"))
            .args([
                "-v",
                "quiet", // 静默模式
                "-print_format",
                "json",          // JSON输出
                "-show_format",  // 显示格式信息
                "-show_streams", // 显示流信息
                &file_path_str,
            ])
            .output()
            .await;

        match result {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
                    bail!("{}文件格式验证失败: {}", file_type, stderr);
                }

                // 检查输出是否包含有效的流信息
                let stdout = str::from_utf8(&output.stdout).unwrap_or("");
                if stdout.len() < 50 || !stdout.contains("streams") {
                    bail!("{}文件缺少有效的媒体流信息", file_type);
                }
            }
            Err(e) => {
                warn!("ffprobe不可用，跳过高级验证: {:#}", e);
                // 如果ffprobe不可用，只做基本的文件大小检查
            }
        }

        Ok(())
    }
}

/// 是否为音视频流临时文件（`.tmp_video` / `.tmp_audio`）。
/// 只有这类文件下载成功后保留断点续传状态（供音频失败/重试复用）；
/// 封面、头像等普通资源成功后应清理状态，避免残留 `.resume` 文件。
fn is_media_stream_tmp_path(path: &Path) -> bool {
    let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
    name.ends_with(".tmp_video") || name.ends_with(".tmp_audio")
}

fn merged_output_size_is_complete(video_size: u64, output_size: u64) -> bool {
    output_size as f64 >= video_size as f64 * 0.98
}

/// 断点续传状态文件路径：目标文件 + `.resume` 后缀。
pub(crate) fn resume_sidecar_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(RESUME_SIDECAR_SUFFIX);
    PathBuf::from(os)
}

/// googlevideo 内容指纹里参与拼接的 query 参数（固定顺序）。
///
/// googlevideo 直链的 path 恒为 `/videoplayback`，只看 path 会让所有视频/清晰度
/// 撞到同一个指纹，续传时就会续到错误内容；而 `expire`/`sig`/`n`/`ei`/`ip`/`mt`/
/// `fvip`/`c`/`txp`/`pot`/`lsparams`/`lsig`/`sparams`/`host` 这些签名与时效参数
/// 每次刷新都会变，不能进指纹。`id`/`itag`/`mh`/`mn` 是内容标识，刷新后保持不变。
const GOOGLEVIDEO_FINGERPRINT_PARAMS: [&str; 4] = ["id", "itag", "mh", "mn"];

/// googlevideo 内容指纹：`googlevideo:id=…&itag=…&mh=…&mn=…`。
/// `mn` 是 CDN 节点列表（逗号分隔），节点顺序可能随直链刷新变化，先排序再拼接。
/// 缺少 id/itag 时返回 `None`，退回 path 指纹（与旧行为一致）。
fn googlevideo_content_fingerprint(parsed: &Url) -> Option<String> {
    let mut parts = Vec::with_capacity(GOOGLEVIDEO_FINGERPRINT_PARAMS.len());
    for key in GOOGLEVIDEO_FINGERPRINT_PARAMS {
        let Some((_, value)) = parsed.query_pairs().find(|(name, _)| name == key) else {
            continue;
        };
        let value = if key == "mn" {
            let value = value.into_owned();
            let mut nodes: Vec<&str> = value.split(',').filter(|node| !node.trim().is_empty()).collect();
            nodes.sort_unstable();
            nodes.join(",")
        } else {
            value.into_owned()
        };
        parts.push(format!("{key}={value}"));
    }
    if parts.len() < 2 {
        return None;
    }
    Some(format!("googlevideo:{}", parts.join("&")))
}

/// 内容指纹：普通 CDN 取 URL 的路径部分（不含域名与签名参数），同一媒体在不同
/// CDN 节点、不同签名/过期时间下路径保持稳定；googlevideo 的 path 恒为
/// `/videoplayback`，改用 query 里的内容标识参数（见 [`GOOGLEVIDEO_FINGERPRINT_PARAMS`]）。
/// 内容变更（如清晰度变化）时指纹变化，可据此使旧断点状态失效，避免续传续到错误内容。
fn url_path_fingerprint(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return url.to_string();
    };
    if is_googlevideo_url(url) {
        return googlevideo_content_fingerprint(&parsed).unwrap_or_else(|| parsed.path().to_string());
    }
    parsed.path().to_string()
}

fn parse_range_line(line: &str) -> Option<(u64, u64)> {
    let mut it = line.splitn(2, ',');
    let start = it.next()?.trim().parse::<u64>().ok()?;
    let end = it.next()?.trim().parse::<u64>().ok()?;
    Some((start, end))
}

/// 读取断点续传状态：首行为内容指纹，后续每行为一个已完成分片 `start,end`。
/// 指纹不匹配（内容已变化）时视为失效并清空状态。
async fn load_completed_ranges(sidecar: &Path, url: &str) -> HashSet<(u64, u64)> {
    let mut result = HashSet::new();
    let content = match fs::read_to_string(sidecar).await {
        Ok(c) => c,
        Err(_) => return result,
    };
    let fingerprint = url_path_fingerprint(url);
    let mut lines = content.lines();
    match lines.next() {
        Some(line) if line.trim() == fingerprint => {}
        _ => {
            let _ = fs::remove_file(sidecar).await;
            return result;
        }
    }
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((start, end)) = parse_range_line(line) {
            result.insert((start, end));
        }
    }
    result
}

/// 确保状态文件首行是指纹行（新文件或指纹变化时重建）。
async fn ensure_resume_fingerprint(sidecar: &Path, url: &str) {
    let fingerprint = url_path_fingerprint(url);
    let content = fs::read_to_string(sidecar).await.unwrap_or_default();
    if !content.lines().next().is_some_and(|line| line.trim() == fingerprint) {
        let _ = fs::write(sidecar, format!("{fingerprint}\n")).await;
    }
}

/// 追加一个已完成分片（`start,end`），供断点续传跳过。
async fn append_completed_range(sidecar: &Path, start: u64, end: u64) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(sidecar).await {
        let _ = file.write_all(format!("{start},{end}\n").as_bytes()).await;
        let _ = file.flush().await;
    }
}

async fn remove_sidecar(sidecar: &Path) {
    let _ = fs::remove_file(sidecar).await;
}

/// 计算单个文件实际使用的并行连接数。
///
/// YouTube/GoogleVideo 使用独立的「单文件连接数」配置（默认 16），不再与下载
/// 线程数取较小值，上限为 [`MAX_PARALLEL_CONNECTIONS`]；其它平台仍使用下载
/// 线程数。任何情况下返回值至少为 1。
fn parallel_file_concurrency(threads: usize, googlevideo_max_connections: usize, is_googlevideo: bool) -> usize {
    if is_googlevideo {
        googlevideo_max_connections.clamp(1, MAX_PARALLEL_CONNECTIONS)
    } else {
        threads.max(1)
    }
}

fn build_parallel_ranges(
    total_size: u64,
    threads: usize,
    is_googlevideo: bool,
    googlevideo_segment: u64,
    max_googlevideo_connections: usize,
) -> (usize, Vec<(u64, u64)>) {
    if total_size == 0 {
        return (1, Vec::new());
    }

    let concurrency = parallel_file_concurrency(threads, max_googlevideo_connections, is_googlevideo);

    if is_googlevideo {
        let mut ranges = Vec::new();
        let mut start = 0u64;
        while start < total_size {
            let end = start.saturating_add(googlevideo_segment - 1).min(total_size - 1);
            ranges.push((start, end));
            start = end.saturating_add(1);
        }
        return (concurrency, ranges);
    }

    let max_segments = total_size.div_ceil(MIN_SEGMENT_SIZE) as usize;
    // 大文件不按「并发数」均分，而是限制单个分片的大小上限：限速 CDN 上小分片
    // 才能靠断点续传逐步推进（见 DEFAULT_SEGMENT_SIZE 的说明）。
    let max_by_size = total_size.div_ceil(DEFAULT_SEGMENT_SIZE) as usize;
    let segment_count = concurrency.max(max_by_size).min(max_segments).max(1);
    let base = total_size / segment_count as u64;
    let mut ranges = Vec::with_capacity(segment_count);
    let mut start = 0u64;
    for index in 0..segment_count {
        let end = if index == segment_count - 1 {
            total_size - 1
        } else {
            start + base - 1
        };
        ranges.push((start, end));
        start = end + 1;
    }
    (concurrency, ranges)
}

struct DownloadRangeInput<'a> {
    client: Client,
    url: &'a str,
    path: &'a Path,
    start: u64,
    end: u64,
    attempts: usize,
    referer: Option<&'a str>,
    cookie: Option<&'a str>,
}

async fn download_range_to_file_with_retry(p: DownloadRangeInput<'_>) -> Result<u64> {
    let DownloadRangeInput {
        client,
        url,
        path,
        start,
        end,
        attempts,
        referer,
        cookie,
    } = p;
    let mut last_error = None;
    for attempt in 1..=attempts.max(1) {
        match download_range_to_file(client.clone(), url, path, start, end, referer, cookie).await {
            Ok(downloaded) => return Ok(downloaded),
            Err(error) if is_certificate_name_mismatch_error(&error) => return Err(error),
            Err(error) => {
                // 分片失败后会重试并由上层断点续传/回退处理，属预期情况；
                // 仅记录 debug，避免 CDN 掐流时刷屏 warn。
                debug!(
                    start,
                    end,
                    attempt,
                    attempts,
                    error = %error,
                    "Range 分片下载失败，准备重试当前分片"
                );
                last_error = Some(error);
                if attempt < attempts {
                    // 指数退避：限速/CDN 掐流时立刻重试只会继续被拒。
                    let backoff_ms = 400u64 * (1u64 << (attempt as u32 - 1).min(5));
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Range 分片下载失败")))
        .with_context(|| format!("Range 分片在重试后仍失败: bytes={start}-{end}"))
}

async fn download_range_to_file(
    client: Client,
    url: &str,
    path: &Path,
    start: u64,
    end: u64,
    referer: Option<&str>,
    cookie: Option<&str>,
) -> Result<u64> {
    let expected = end.saturating_sub(start) + 1;

    let mut file = OpenOptions::new().write(true).open(path).await?;
    let progress_path = path.display().to_string();
    let mut received = 0u64;
    let mut request_index = 0u32;
    let mut stalled_requests = 0u32;

    // 分片内断流续传：读流中途断开（CDN 掐流/连接被重置/等待数据超时）时，已写入的
    // 字节保留，下一次请求从 `start + received` 继续，而不是整片重下。
    // 硬失败（连接被拒/500/首字节超时）在下面直接返回，交给外层
    // `download_range_to_file_with_retry` 重试整个分片，避免内层无限重试放大外层次数。
    loop {
        request_index += 1;
        let request_start = start.saturating_add(received);
        let range_value = format!("bytes={}-{}", request_start, end);
        let request = client
            .media_request(Method::GET, url)
            .header(header::RANGE, range_value)
            .header(header::ACCEPT_ENCODING, "identity");
        let request = if let Some(referer) = referer {
            request.header(header::REFERER, referer)
        } else {
            request
        };
        let request = if let Some(cookie) = cookie {
            request.header(header::COOKIE, cookie)
        } else {
            request
        };
        let resp = tokio::time::timeout(FIRST_BYTE_TIMEOUT, request.send())
            .await
            .map_err(|_| anyhow!("Range分片响应超时（{} 秒）", FIRST_BYTE_TIMEOUT.as_secs()))?
            .context("Range下载请求失败")?;

        ensure!(
            resp.status() == StatusCode::PARTIAL_CONTENT,
            "Range响应异常: {}",
            resp.status()
        );

        let resp = resp.error_for_status().context("Range状态码错误")?;

        file.seek(std::io::SeekFrom::Start(request_start)).await?;
        let mut stream = resp.bytes_stream();
        let mut first_chunk = true;
        // 进度：分片下载过程中按约 250ms 节流上报「文件内绝对偏移」，
        // 让首页进度条平滑增长而不是等整个分片下完才跳一次。
        let mut last_progress_report = std::time::Instant::now();
        let received_before_request = received;
        let mut interrupted = false;
        loop {
            let wait = if first_chunk {
                FIRST_BYTE_TIMEOUT
            } else {
                CHUNK_IDLE_TIMEOUT
            };
            match tokio::time::timeout(wait, stream.next()).await {
                // 首字节就超时：连接没起来，属于硬失败，直接交给外层重试。
                Err(_) if first_chunk => {
                    return Err(anyhow!(
                        "Range 分片 bytes={}-{} 等待首字节超时（{} 秒）",
                        request_start,
                        end,
                        FIRST_BYTE_TIMEOUT.as_secs()
                    ));
                }
                // 中途等待数据超时：按断流处理，从已写位置继续。
                Err(_) => {
                    interrupted = true;
                    break;
                }
                Ok(None) => break,
                Ok(Some(Ok(chunk))) => {
                    file.write_all(&chunk).await?;
                    received += chunk.len() as u64;
                    first_chunk = false;
                    if last_progress_report.elapsed() >= Duration::from_millis(250) {
                        last_progress_report = std::time::Instant::now();
                        crate::download_progress::DOWNLOAD_PROGRESS
                            .report_bytes(&progress_path, start.saturating_add(received))
                            .await;
                    }
                }
                Ok(Some(Err(error))) if received >= expected && contains_tls_close_notify_eof(&error.to_string()) => {
                    warn!(
                        "Range CDN 未发送 TLS close_notify，但分片已完整接收: received={} expected={}",
                        received, expected
                    );
                    break;
                }
                Ok(Some(Err(error))) => {
                    // 证书不匹配属于环境/配置问题，重试没有意义，直接上抛。
                    let error = anyhow::Error::from(error);
                    if is_certificate_name_mismatch_error(&error) {
                        return Err(error);
                    }
                    interrupted = true;
                    break;
                }
            }
        }

        if received >= expected {
            break;
        }

        // 走到这里说明本轮 Range 请求没把分片收完，且不是硬失败：按断流续传。
        // 连续多次一个字节都没推进就放弃，防止 CDN 一直掐流时内层死循环。
        if received > received_before_request {
            stalled_requests = 0;
        } else {
            stalled_requests += 1;
        }
        debug!(
            start,
            end, request_start, received, interrupted, request_index, "Range 分片中途断流，准备从已写入位置继续下载"
        );
        if stalled_requests >= RANGE_RESUME_MAX_STALLED {
            return Err(anyhow!(
                "Range 分片 bytes={}-{} 连续 {} 次续传无进展，放弃当前分片",
                start,
                end,
                RANGE_RESUME_MAX_STALLED
            ));
        }
        if request_index >= RANGE_RESUME_MAX_REQUESTS {
            return Err(anyhow!(
                "Range 分片 bytes={}-{} 续传请求次数达到上限（{}）",
                start,
                end,
                RANGE_RESUME_MAX_REQUESTS
            ));
        }
        tokio::time::sleep(RANGE_RESUME_BACKOFF).await;
    }

    // 分片完整接收：上报最终偏移（与分片完成时的累计上报一致，取 max）
    crate::download_progress::DOWNLOAD_PROGRESS
        .report_bytes(&progress_path, start.saturating_add(received))
        .await;
    file.flush().await?;

    ensure!(
        received == expected,
        "Range分片下载不完整: received {} bytes, expected {} bytes",
        received,
        expected
    );

    Ok(received)
}

trait ResponseExt {
    fn header_content_length(&self) -> Option<u64>;
    fn header_file_size(&self) -> Option<u64>;
}

impl ResponseExt for reqwest::Response {
    fn header_content_length(&self) -> Option<u64> {
        self.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
    }

    fn header_file_size(&self) -> Option<u64> {
        self.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.rsplit_once('/'))
            .and_then(|(_, size_str)| size_str.parse::<u64>().ok())
    }
}

pub async fn remux_with_ffmpeg(input_path: &Path, output_path: &Path) -> Result<()> {
    // 确保输出目录存在
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).await?;
        }
    }

    // 将Path转换为字符串，防止临时值过早释放
    let input_path_str = input_path.to_string_lossy().to_string();
    let output_path_str = output_path.to_string_lossy().to_string();

    let args = [
        "-i",
        &input_path_str,
        "-c",
        "copy",
        "-movflags",
        "+faststart",
        "-y",
        &output_path_str,
    ];

    let output = crate::utils::process::tokio_command(resolve_media_tool_path("ffmpeg"))
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
        bail!("ffmpeg error: {}", stderr.trim());
    }

    Ok(())
}

fn unique_temp_path_for_media(input_path: &Path, label: &str, extension: &str) -> PathBuf {
    let parent = input_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = input_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "media".into());
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    parent.join(format!(".{}.{}.{}.{}", file_name, label, timestamp_ms, extension))
}

fn audio_codec_needs_mp4_muxer_for_m4a(codec_name: &str) -> bool {
    codec_name.eq_ignore_ascii_case("flac")
}

fn is_ipod_muxer_flac_unsupported_error(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("ipod") && stderr.contains("codec flac") && stderr.contains("not currently supported in container")
}

async fn probe_primary_audio_codec(audio_path: &Path) -> Option<String> {
    let audio_path_str = audio_path.to_string_lossy().to_string();
    let output = match crate::utils::process::tokio_command(resolve_media_tool_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &audio_path_str,
        ])
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) => {
            debug!(
                "探测音频编码失败，封面内嵌将使用默认 M4A muxer: {}，error={:#}",
                audio_path.display(),
                err
            );
            return None;
        }
    };

    if !output.status.success() {
        let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
        debug!(
            "探测音频编码失败，封面内嵌将使用默认 M4A muxer: {}，ffprobe={}",
            audio_path.display(),
            stderr.trim()
        );
        return None;
    }

    str::from_utf8(&output.stdout)
        .ok()
        .and_then(|stdout| stdout.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(|codec| codec.to_ascii_lowercase())
}

fn build_embed_cover_args(audio_path: &str, cover_path: &str, output_path: &str, force_mp4_muxer: bool) -> Vec<String> {
    // M4A/MP4 容器内嵌封面使用 attached_pic 视频流。
    // 只映射原文件音频流和新的封面图，避免重复保留旧封面或误把混合流视频也写回 m4a。
    // 音频流直接 copy；封面统一转成 mjpeg，兼容实际下载到 PNG/WebP 但文件名仍是 .jpg 的情况。
    let mut args = vec![
        "-i".to_string(),
        audio_path.to_string(),
        "-i".to_string(),
        cover_path.to_string(),
        "-map".to_string(),
        "0:a".to_string(),
        "-map".to_string(),
        "1:v:0".to_string(),
        "-map_metadata".to_string(),
        "0".to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
        "-c:v".to_string(),
        "mjpeg".to_string(),
        "-q:v".to_string(),
        "2".to_string(),
        "-disposition:v:0".to_string(),
        "attached_pic".to_string(),
        "-metadata:s:v".to_string(),
        "title=Album cover".to_string(),
        "-metadata:s:v".to_string(),
        "comment=Cover (front)".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
    ];

    // .m4a 后缀会让 ffmpeg 默认使用 ipod muxer；ipod muxer 不支持 FLAC。
    // B 站的无损音频常见为“FLAC 音频流 + .m4a/MP4 容器”，因此这类文件
    // 内嵌封面时强制使用 mp4 muxer，避免每个分P都报
    // "codec flac ... not currently supported in container"。
    if force_mp4_muxer {
        args.push("-f".to_string());
        args.push("mp4".to_string());
    }

    args.push("-y".to_string());
    args.push(output_path.to_string());
    args
}

pub async fn embed_cover_into_m4a_with_ffmpeg(audio_path: &Path, cover_path: &Path) -> Result<()> {
    ensure!(
        tokio::fs::metadata(audio_path).await.is_ok(),
        "音频文件不存在: {}",
        audio_path.display()
    );
    ensure!(
        tokio::fs::metadata(cover_path).await.is_ok(),
        "封面文件不存在: {}",
        cover_path.display()
    );

    let tmp_output_path = unique_temp_path_for_media(audio_path, "cover", "m4a");
    let backup_path = unique_temp_path_for_media(audio_path, "backup", "m4a");

    let audio_path_str = audio_path.to_string_lossy().to_string();
    let cover_path_str = cover_path.to_string_lossy().to_string();
    let tmp_output_path_str = tmp_output_path.to_string_lossy().to_string();

    let audio_codec = probe_primary_audio_codec(audio_path).await;
    let force_mp4_muxer = audio_codec.as_deref().is_some_and(audio_codec_needs_mp4_muxer_for_m4a);
    if force_mp4_muxer {
        debug!(
            "检测到 FLAC-in-M4A 音频，封面内嵌改用 MP4 muxer: {}",
            audio_path.display()
        );
    }

    let args = build_embed_cover_args(&audio_path_str, &cover_path_str, &tmp_output_path_str, force_mp4_muxer);

    let mut output = crate::utils::process::tokio_command(resolve_media_tool_path("ffmpeg"))
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown").to_string();
        let _ = fs::remove_file(&tmp_output_path).await;
        if !force_mp4_muxer && is_ipod_muxer_flac_unsupported_error(&stderr) {
            debug!(
                "默认 M4A/ipod muxer 不支持 FLAC，封面内嵌改用 MP4 muxer 重试: {}",
                audio_path.display()
            );
            let retry_args = build_embed_cover_args(&audio_path_str, &cover_path_str, &tmp_output_path_str, true);
            output = crate::utils::process::tokio_command(resolve_media_tool_path("ffmpeg"))
                .args(retry_args)
                .output()
                .await?;
        } else {
            bail!("ffmpeg cover embed error: {}", stderr.trim());
        }
    }

    if !output.status.success() {
        let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
        let _ = fs::remove_file(&tmp_output_path).await;
        bail!("ffmpeg cover embed error: {}", stderr.trim());
    }

    let output_size = tokio::fs::metadata(&tmp_output_path)
        .await
        .with_context(|| format!("无法读取封面内嵌后的临时文件: {}", tmp_output_path.display()))?
        .len();
    ensure!(
        output_size > 0,
        "封面内嵌后的临时文件为空: {}",
        tmp_output_path.display()
    );

    fs::rename(audio_path, &backup_path)
        .await
        .with_context(|| format!("备份原音频文件失败: {}", audio_path.display()))?;
    if let Err(err) = fs::rename(&tmp_output_path, audio_path).await {
        if let Err(restore_err) = fs::rename(&backup_path, audio_path).await {
            error!(
                "封面内嵌失败后恢复原音频文件也失败: {} -> {}, error={:#}",
                backup_path.display(),
                audio_path.display(),
                restore_err
            );
        }
        return Err(err).with_context(|| format!("替换封面内嵌后的音频文件失败: {}", audio_path.display()));
    }

    let _ = fs::remove_file(&backup_path).await;
    Ok(())
}

pub async fn split_media_segments_with_ffmpeg(
    input_path: &Path,
    output_paths: &[PathBuf],
    split_points_seconds: &[u32],
) -> Result<()> {
    ensure!(!output_paths.is_empty(), "segment output paths must not be empty");

    for output_path in output_paths {
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).await?;
        }
    }

    if split_points_seconds.is_empty() {
        if output_paths[0].exists() {
            fs::remove_file(&output_paths[0]).await?;
        }
        fs::copy(input_path, &output_paths[0]).await?;
        return Ok(());
    }

    let output_dir = output_paths[0]
        .parent()
        .context("segment output path must have a parent directory")?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_dir = output_dir.join(format!(".bili-sync-chapters-{}-{}", std::process::id(), ts));
    fs::create_dir_all(&temp_dir).await?;

    let input_path_str = input_path.to_string_lossy().to_string();
    let ext = output_paths[0]
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("mp4");
    let output_pattern = temp_dir.join(format!("segment-%03d.{ext}"));
    let output_pattern_str = output_pattern.to_string_lossy().to_string();
    let split_points_arg = split_points_seconds
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");

    let output = crate::utils::process::tokio_command(resolve_media_tool_path("ffmpeg"))
        .args([
            "-y",
            "-i",
            &input_path_str,
            "-map",
            "0",
            "-c",
            "copy",
            "-f",
            "segment",
            "-segment_times",
            &split_points_arg,
            "-reset_timestamps",
            "1",
            "-break_non_keyframes",
            "1",
            &output_pattern_str,
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
        let _ = fs::remove_dir_all(&temp_dir).await;
        bail!("ffmpeg chapter split error: {}", stderr.trim());
    }

    let mut segment_paths = Vec::new();
    let mut entries = fs::read_dir(&temp_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file() {
            segment_paths.push(entry.path());
        }
    }
    segment_paths.sort();

    if segment_paths.len() != output_paths.len() {
        let _ = fs::remove_dir_all(&temp_dir).await;
        bail!(
            "ffmpeg generated {} chapter segments, expected {}",
            segment_paths.len(),
            output_paths.len()
        );
    }

    for (segment_path, output_path) in segment_paths.into_iter().zip(output_paths) {
        if output_path.exists() {
            fs::remove_file(output_path).await?;
        }
        fs::rename(segment_path, output_path).await?;
    }

    let _ = fs::remove_dir_all(&temp_dir).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_certificate_name_mismatch_error_text() {
        let err = anyhow!(
            "error sending request: client error (Connect): invalid peer certificate: certificate not valid for name \"upos-sz-mirror14b.bilivideo.com\""
        );

        assert!(is_certificate_name_mismatch_error(&err));
    }

    #[test]
    fn marks_same_host_as_temporarily_blocked() {
        BAD_CDN_HOSTS.lock().unwrap_or_else(|e| e.into_inner()).clear();

        let err =
            anyhow!("invalid peer certificate: certificate not valid for name \"upos-sz-mirror14b.bilivideo.com\"");
        mark_bad_cdn_host("https://upos-sz-mirror14b.bilivideo.com/video.m4s", &err);

        assert!(is_url_blocked_by_bad_cdn_host(
            "https://upos-sz-mirror14b.bilivideo.com/audio.m4s"
        ));
        assert!(!is_url_blocked_by_bad_cdn_host(
            "https://upos-sz-mirror08c.bilivideo.com/audio.m4s"
        ));
    }

    #[test]
    fn detects_download_error_that_should_refresh_playurl() {
        let err = anyhow!("failed to download from [\"https://cdn.example/video.m4s\"]: 所有URL尝试失败");

        assert!(should_refresh_playurl_after_download_error(&err));
    }

    #[test]
    fn small_or_unbounded_sidecar_uses_expected_single_connection_fallback() {
        assert!(is_expected_single_connection_fallback(&anyhow!(
            "文件过小(109814 bytes)，不启用分片下载"
        )));
        assert!(is_expected_single_connection_fallback(&anyhow!("无法获取文件大小")));
        assert!(!is_expected_single_connection_fallback(&anyhow!(
            "Range 分片下载不完整"
        )));
    }

    #[test]
    fn merged_output_must_keep_nearly_all_video_bytes() {
        let video_size = 2_047 * 1024 * 1024;
        assert!(merged_output_size_is_complete(video_size, video_size));
        assert!(merged_output_size_is_complete(
            video_size,
            video_size.saturating_mul(99) / 100
        ));
        // Issue #254：2GB 视频流合并后只剩 1.7GB，不能被当作完成
        assert!(!merged_output_size_is_complete(video_size, 1700 * 1024 * 1024));
    }

    #[test]
    fn googlevideo_uses_small_ranges_with_bounded_native_concurrency() {
        let total_size = 18 * 1024 * 1024;
        let (concurrency, ranges) = build_parallel_ranges(total_size, 16, true, GOOGLEVIDEO_SEGMENT_SIZE, 16);

        assert_eq!(concurrency, 16);
        assert_eq!(ranges.len(), 5);
        assert_eq!(ranges[0], (0, GOOGLEVIDEO_SEGMENT_SIZE - 1));
        assert_eq!(ranges[3], (12 * 1024 * 1024, 16 * 1024 * 1024 - 1));
        assert_eq!(ranges[4], (16 * 1024 * 1024, total_size - 1));
    }

    #[test]
    fn parallel_file_concurrency_uses_independent_youtube_limit() {
        // YouTube 单文件连接数独立生效，不再与下载线程数取较小值
        assert_eq!(parallel_file_concurrency(4, 16, true), 16);
        assert_eq!(parallel_file_concurrency(1, 16, true), 16);
        assert_eq!(parallel_file_concurrency(64, 64, true), 32);
        assert_eq!(parallel_file_concurrency(8, 4, true), 4);
        assert_eq!(parallel_file_concurrency(0, 0, true), 1);
        // 其它平台仍使用下载线程数
        assert_eq!(parallel_file_concurrency(4, 16, false), 4);
        assert_eq!(parallel_file_concurrency(0, 0, false), 1);
    }

    #[test]
    fn googlevideo_ranges_use_independent_connection_limit() {
        let total_size = 64 * 1024 * 1024;
        // 「下载线程数」= 8、YouTube 单文件连接数 = 16 时，YouTube 用 16 条连接
        // （旧版取 min(8, 16) = 8，调大 YouTube 配置也压不过线程数）。
        let (concurrency, _) = build_parallel_ranges(total_size, 8, true, GOOGLEVIDEO_SEGMENT_SIZE, 16);
        assert_eq!(concurrency, 16);

        // 配置的 YouTube 连接数低于线程数时按配置收敛
        let (concurrency, _) = build_parallel_ranges(total_size, 8, true, GOOGLEVIDEO_SEGMENT_SIZE, 4);
        assert_eq!(concurrency, 4);

        let (concurrency, _) = build_parallel_ranges(total_size, 64, true, GOOGLEVIDEO_SEGMENT_SIZE, 16);
        assert_eq!(concurrency, 16);
        let (concurrency, _) = build_parallel_ranges(total_size, 64, true, GOOGLEVIDEO_SEGMENT_SIZE, 32);
        assert_eq!(concurrency, 32);
        // 非法 0 值也不能让并发变成 0
        let (concurrency, _) = build_parallel_ranges(total_size, 64, true, GOOGLEVIDEO_SEGMENT_SIZE, 0);
        assert_eq!(concurrency, 1);
        // 下载线程数为 1 时 YouTube 也不能退化成单连接
        let (concurrency, _) = build_parallel_ranges(total_size, 1, true, GOOGLEVIDEO_SEGMENT_SIZE, 16);
        assert_eq!(concurrency, 16);
    }

    #[test]
    fn googlevideo_audio_uses_1mib_ranges() {
        // 短音频（2.88MB）按音频 1MiB 分片应拆成 3 段有界 Range，而不是
        // 单个 4MiB 分片（部分节点音频 Range 上限 1MiB，超限 403）。
        let total_size = 2_884_577;
        let (_, ranges) = build_parallel_ranges(total_size, 4, true, MIN_SEGMENT_SIZE, 16);

        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0], (0, MIN_SEGMENT_SIZE - 1));
        assert_eq!(ranges[1], (MIN_SEGMENT_SIZE, 2 * MIN_SEGMENT_SIZE - 1));
        assert_eq!(ranges[2], (2 * MIN_SEGMENT_SIZE, total_size - 1));
    }

    #[test]
    fn googlevideo_alternate_hosts_are_discovered() {
        let url = "https://rr5---sn-oguesndz.googlevideo.com/videoplayback?mn=sn-oguesndz%2Csn-oguelnl7&mime=audio%2Fmp4&itag=140";
        let candidates = googlevideo_alternate_urls(url);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0], url);
        assert!(
            candidates[1].starts_with("https://rr5---sn-oguelnl7.googlevideo.com/"),
            "备用节点 URL 异常: {}",
            candidates[1]
        );
    }

    #[test]
    fn regular_cdn_keeps_configured_parallel_range_count() {
        let total_size = 64 * 1024 * 1024;
        let (concurrency, ranges) = build_parallel_ranges(total_size, 8, false, 0, 16);

        assert_eq!(concurrency, 8);
        assert_eq!(ranges.len(), 8);
        assert_eq!(ranges[0], (0, 8 * 1024 * 1024 - 1));
        assert_eq!(ranges[7], (56 * 1024 * 1024, total_size - 1));
    }

    #[test]
    fn large_cdn_file_is_split_into_small_segments() {
        // 抖音 4K60 原画单集就有 7GB：按并发数均分会切出 ~460MB 的大分片，
        // 限速 CDN 上一个分片走不完就一直失败。分片必须限制在上限以内。
        let total_size = 7_375_161_692u64;
        let (concurrency, ranges) = build_parallel_ranges(total_size, 16, false, 0, 16);

        assert_eq!(concurrency, 16, "并发数不应被放大");
        let expected = total_size.div_ceil(DEFAULT_SEGMENT_SIZE) as usize;
        assert_eq!(ranges.len(), expected);
        // 除最后一片外都不超过上限
        for range in &ranges[..ranges.len() - 1] {
            let len = range.1 - range.0 + 1;
            assert!(
                len <= DEFAULT_SEGMENT_SIZE,
                "分片 {} 超过上限 {}",
                len,
                DEFAULT_SEGMENT_SIZE
            );
        }
        // 分片必须首尾相接铺满整个文件
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[ranges.len() - 1].1, total_size - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1 + 1, pair[1].0);
        }
    }

    #[test]
    fn flac_audio_uses_mp4_muxer_for_m4a_cover_embedding() {
        assert!(audio_codec_needs_mp4_muxer_for_m4a("flac"));
        assert!(audio_codec_needs_mp4_muxer_for_m4a("FLAC"));
        assert!(!audio_codec_needs_mp4_muxer_for_m4a("aac"));
    }

    #[test]
    fn detects_ipod_muxer_flac_unsupported_error() {
        let stderr =
            "[ipod @ 0x123] Could not find tag for codec flac in stream #0, codec not currently supported in container";

        assert!(is_ipod_muxer_flac_unsupported_error(stderr));
        assert!(!is_ipod_muxer_flac_unsupported_error("some other ffmpeg error"));
    }

    #[test]
    fn embed_cover_args_force_mp4_muxer_before_output_path() {
        let args = build_embed_cover_args("audio.m4a", "cover.jpg", "out.m4a", true);

        let muxer_index = args
            .iter()
            .position(|arg| arg == "-f")
            .expect("should set output muxer");
        assert_eq!(args.get(muxer_index + 1).map(String::as_str), Some("mp4"));
        assert!(
            muxer_index < args.len() - 1,
            "output muxer must be specified before output path"
        );
        assert_eq!(args.last().map(String::as_str), Some("out.m4a"));
    }

    #[test]
    fn embed_cover_args_keep_default_muxer_for_regular_m4a() {
        let args = build_embed_cover_args("audio.m4a", "cover.jpg", "out.m4a", false);

        assert!(!args.iter().any(|arg| arg == "-f"));
        assert_eq!(args.last().map(String::as_str), Some("out.m4a"));
    }

    #[test]
    fn resume_sidecar_path_appends_suffix() {
        let path = Path::new("/tmp/video/P01.P1.tmp_video");
        assert_eq!(
            resume_sidecar_path(path).to_string_lossy(),
            "/tmp/video/P01.P1.tmp_video.resume"
        );
    }

    #[test]
    fn url_path_fingerprint_ignores_host_and_query() {
        let a = url_path_fingerprint(
            "https://xy1.mcdn.bilivideo.cn:8082/v1/resource/upgcxcode/15/52/x.m4s?deadline=100&sign=abc",
        );
        let b = url_path_fingerprint(
            "https://xy2.mcdn.bilivideo.cn/v1/resource/upgcxcode/15/52/x.m4s?deadline=200&sign=xyz",
        );
        assert_eq!(a, "/v1/resource/upgcxcode/15/52/x.m4s");
        assert_eq!(a, b);
        assert_ne!(a, url_path_fingerprint("https://cdn.example/v1/resource/other.m4s"));
    }

    #[test]
    fn parse_range_line_accepts_valid_and_rejects_bad() {
        assert_eq!(parse_range_line("0,1048575"), Some((0, 1048575)));
        assert_eq!(parse_range_line(" 1048576 , 2097151 "), Some((1048576, 2097151)));
        assert_eq!(parse_range_line("abc,123"), None);
        assert_eq!(parse_range_line("123"), None);
        assert_eq!(parse_range_line(""), None);
    }

    /// 本地 Range 测试服务器：支持 HEAD / 206 Range，可指定某个分片持续失败，
    /// 并记录每个收到的 Range 请求，用于验证断点续传只补缺失分片。
    async fn spawn_range_test_server(
        data: Vec<u8>,
        drop_on_start: Option<u64>,
        url_path: &'static str,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<(u64, u64)>>>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requested = Arc::new(Mutex::new(Vec::new()));
        let fail_start = Arc::new(AtomicUsize::new(usize::MAX));
        let fail_left = Arc::new(AtomicUsize::new(0));
        let data_len = data.len() as u64;

        let requested_ret = requested.clone();
        let fail_start_ret = fail_start.clone();
        let fail_left_ret = fail_left.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let data = data.clone();
                let requested = requested.clone();
                let fail_start = fail_start.clone();
                let fail_left = fail_left.clone();
                let drop_on_start = drop_on_start;
                tokio::spawn(async move {
                    let mut socket = socket;
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
                    loop {
                        let n = match socket.read(&mut tmp).await {
                            Ok(n) if n > 0 => n,
                            _ => return,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let first_line = text.lines().next().unwrap_or("").to_string();
                    let is_head = first_line.starts_with("HEAD");
                    let start_end: Option<(u64, u64)> = text.lines().find_map(|line| {
                        let line = line.trim_start();
                        let lowered = line.to_ascii_lowercase();
                        let value = lowered.strip_prefix("range: bytes=")?;
                        let mut it = value.split('-');
                        let a = it.next()?.trim().parse::<u64>().ok()?;
                        let b = it.next()?.trim().parse::<u64>().ok()?;
                        Some((a, b))
                    });

                    if let Some((s, e)) = start_end {
                        requested.lock().unwrap().push((s, e));
                        if Some(s) == drop_on_start {
                            // 模拟 CDN 节点挂掉/断网：不响应、直接断开连接并停止服务
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            let _ = socket.shutdown().await;
                            return;
                        }
                        if s as usize == fail_start.load(Ordering::SeqCst) {
                            let left = fail_left.load(Ordering::SeqCst);
                            if left > 0 {
                                fail_left.store(left - 1, Ordering::SeqCst);
                                let _ = socket
                                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                    .await;
                                let _ = socket.shutdown().await;
                                return;
                            }
                        }
                    }

                    if is_head {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            data_len
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                    } else if let Some((s, e)) = start_end {
                        if e < s || e >= data_len {
                            let _ = socket
                                .write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .await;
                        } else {
                            let body = &data[s as usize..=e as usize];
                            let resp = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                                s, e, data_len, body.len()
                            );
                            let _ = socket.write_all(resp.as_bytes()).await;
                            let _ = socket.write_all(body).await;
                        }
                    } else {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            data_len
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.write_all(&data).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });

        (
            format!("http://{}{}", addr, url_path),
            requested_ret,
            fail_start_ret,
            fail_left_ret,
        )
    }

    /// 本地 Range 测试服务器（模拟 CDN 掐流）：首次收到从 0 开始的分片请求时，
    /// 只发送 `truncate_after` 字节就断开连接；后续请求按 Range 正常返回剩余数据。
    /// 返回 URL 与收到的 Range 请求记录。
    async fn spawn_truncating_range_server(
        data: Vec<u8>,
        truncate_after: usize,
        url_path: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<(u64, u64)>>>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requested = Arc::new(Mutex::new(Vec::new()));
        let truncated = Arc::new(AtomicBool::new(false));
        let data_len = data.len() as u64;

        let requested_ret = requested.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let data = data.clone();
                let requested = requested.clone();
                let truncated = truncated.clone();
                tokio::spawn(async move {
                    let mut socket = socket;
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
                    loop {
                        let n = match socket.read(&mut tmp).await {
                            Ok(n) if n > 0 => n,
                            _ => return,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let first_line = text.lines().next().unwrap_or("").to_string();
                    let is_head = first_line.starts_with("HEAD");
                    let start_end: Option<(u64, u64)> = text.lines().find_map(|line| {
                        let line = line.trim_start();
                        let lowered = line.to_ascii_lowercase();
                        let value = lowered.strip_prefix("range: bytes=")?;
                        let mut it = value.split('-');
                        let a = it.next()?.trim().parse::<u64>().ok()?;
                        let b = it.next()?.trim().parse::<u64>().ok()?;
                        Some((a, b))
                    });

                    if is_head {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            data_len
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let Some((s, e)) = start_end else {
                        return;
                    };
                    requested.lock().unwrap().push((s, e));
                    if e < s || e >= data_len {
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let body = &data[s as usize..=e as usize];
                    let resp = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        s,
                        e,
                        data_len,
                        body.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    // 只在真正要截断这一次请求时才置位：并发下载时分片请求顺序不固定，
                    // 若先到的是 s != 0 的分片，不能把截断机会浪费掉。
                    let should_truncate =
                        s == 0 && truncate_after < body.len() && !truncated.swap(true, Ordering::SeqCst);
                    if should_truncate {
                        // 只发一部分 body 就断开，模拟 CDN 中途掐流
                        let _ = socket.write_all(&body[..truncate_after]).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let _ = socket.write_all(body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        (format!("http://{}{}", addr, url_path), requested_ret)
    }

    #[tokio::test]
    async fn parallel_download_resumes_after_range_failures() {
        use std::sync::atomic::Ordering;

        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let (url, requested, fail_start, fail_left) =
            spawn_range_test_server(data.clone(), None, "/res/test.m4s").await;
        let dir = std::env::temp_dir().join(format!("bili-resume-int-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");
        let downloader = Downloader::new(Client::new());

        // 让首个分片 (0..2MB) 持续失败：3 次重试均失败 → 并发下载整体失败
        fail_start.store(0, Ordering::SeqCst);
        fail_left.store(10, Ordering::SeqCst);
        let first = downloader.fetch_parallel(&url, &path, 4, 16, None, None).await;
        assert!(first.is_err(), "分片持续失败时并发下载应返回错误");

        // 断点状态应保留，且首个失败分片未被标记完成
        let sidecar = resume_sidecar_path(&path);
        let completed = load_completed_ranges(&sidecar, &url).await;
        assert!(!completed.is_empty(), "失败后应保留已完成分片状态");
        assert!(!completed.contains(&(0, 2 * 1024 * 1024 - 1)), "失败分片不应被标记完成");

        // 修复 server 后再次下载：应只请求缺失分片并续传成功
        fail_start.store(usize::MAX, Ordering::SeqCst);
        let requested_before = requested.lock().unwrap().len();
        downloader
            .fetch_parallel(&url, &path, 4, 16, None, None)
            .await
            .expect("断点续传应成功");

        // 文件完整且内容与源一致（无空洞、无错位）
        let written = fs::read(&path).await.unwrap();
        assert_eq!(written, data, "续传后文件内容应与源完全一致");

        // 续传只请求了缺失的首个分片
        let second_round: Vec<_> = {
            let reqs = requested.lock().unwrap();
            reqs[requested_before..].to_vec()
        };
        assert!(!second_round.is_empty(), "续传应至少请求一次缺失分片");
        assert!(
            second_round.iter().all(|(s, _)| *s == 0),
            "续传应只请求缺失的首个分片，实际请求: {:?}",
            second_round
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn parallel_download_success_keeps_full_range_state() {
        let data: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let (url, _requested, _fail_start, _fail_left) =
            spawn_range_test_server(data.clone(), None, "/res/test.m4s").await;
        let dir = std::env::temp_dir().join(format!("bili-resume-ok-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");
        let downloader = Downloader::new(Client::new());

        downloader
            .fetch_parallel(&url, &path, 4, 16, None, None)
            .await
            .expect("正常下载应成功");

        let written = fs::read(&path).await.unwrap();
        assert_eq!(written, data);

        // B站路径成功后保留全部 4 个分片状态，供音频失败/重试时续传命中
        let sidecar = resume_sidecar_path(&path);
        let completed = load_completed_ranges(&sidecar, &url).await;
        assert_eq!(completed.len(), 4, "成功下载后应记录全部完成分片");

        let _ = fs::remove_dir_all(&dir).await;
    }

    /// 真实断连→重连：下载中 CDN 节点直接挂掉（连接被断开、服务器停止服务），
    /// 已下分片保留；换新节点重连后只补缺失分片，最终文件逐字节一致。
    #[tokio::test]
    async fn parallel_download_resumes_after_connection_drop() {
        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        // 服务器 A：收到首个分片 (0..2MB) 请求时直接断开连接并停止服务（模拟节点挂掉）
        let (url_a, _requested, _fail_start, _fail_left) =
            spawn_range_test_server(data.clone(), Some(0), "/res/test.m4s").await;
        let dir = std::env::temp_dir().join(format!("bili-resume-drop-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");
        let downloader = Downloader::new(Client::new());

        // 第一次下载：首个分片连接被断开且服务器已停止 → 重试仍失败 → 整体报错
        println!(
            "[1] 开始真实下载 {} ({} MiB, 4 分片), 服务器 A: {}",
            path.display(),
            data.len() / 1024 / 1024,
            url_a
        );
        let first = downloader.fetch_parallel(&url_a, &path, 4, 16, None, None).await;
        assert!(first.is_err(), "节点断开后并发下载应返回错误");
        let first_err = format!("{:#}", first.unwrap_err());
        assert!(first_err.to_lowercase().contains("error"), "错误应来自网络层连接失败");
        println!(
            "[2] 分片 (0..2MiB) 连接被断开、服务器 A 停止服务 -> 下载失败: {}",
            first_err.lines().next().unwrap_or("")
        );

        // 断点状态保留：首个分片未完成，其余分片已完成
        let sidecar = resume_sidecar_path(&path);
        let completed = load_completed_ranges(&sidecar, &url_a).await;
        assert!(!completed.is_empty(), "断连后应保留已完成分片状态");
        assert!(!completed.contains(&(0, 2 * 1024 * 1024 - 1)), "断连分片不应被标记完成");
        println!(
            "[3] 断连后已落盘 {} 个分片状态（首分片未完成）; 文件大小={} MiB",
            completed.len(),
            tokio::fs::metadata(&path)
                .await
                .map(|m| m.len() / 1024 / 1024)
                .unwrap_or(0)
        );

        // 服务器 B：同一内容、同一 URL 路径，换端口（等价换 CDN 节点）重新提供服务
        let (url_b, requested_b, _fs, _fl) = spawn_range_test_server(data.clone(), None, "/res/test.m4s").await;
        println!("[4] 服务器 B(同内容同路径, 换节点) 就绪: {}", url_b);
        downloader
            .fetch_parallel(&url_b, &path, 4, 16, None, None)
            .await
            .expect("换节点重连续传应成功");
        println!(
            "[5] 重连后服务器 B 收到的 Range 请求: {:?}",
            requested_b.lock().unwrap()
        );

        // 最终文件与源逐字节一致
        let written = fs::read(&path).await.unwrap();
        assert_eq!(written, data, "重连续传后文件内容应与源完全一致");

        // 服务器 B 只收到了缺失分片的请求
        let reqs: Vec<_> = {
            let guard = requested_b.lock().unwrap();
            guard.clone()
        };
        assert!(!reqs.is_empty(), "重连后应至少请求一次缺失分片");
        assert!(
            reqs.iter().all(|(s, _)| *s == 0),
            "重连应只请求缺失的首个分片，实际请求: {:?}",
            reqs
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn resume_state_roundtrip_with_fingerprint() {
        let dir = std::env::temp_dir().join(format!("bili-resume-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let sidecar = resume_sidecar_path(&dir.join("video.tmp_video"));
        let url = "https://cdn.example/v1/resource/upgcxcode/1/2/x.m4s?deadline=1&sign=a";

        ensure_resume_fingerprint(&sidecar, url).await;
        append_completed_range(&sidecar, 0, 100).await;
        append_completed_range(&sidecar, 101, 200).await;

        let loaded = load_completed_ranges(&sidecar, url).await;
        assert!(loaded.contains(&(0, 100)));
        assert!(loaded.contains(&(101, 200)));

        // 指纹不一致：状态失效并清空
        let other_url = "https://cdn.example/v1/resource/upgcxcode/9/9/other.m4s?x=1";
        let loaded2 = load_completed_ranges(&sidecar, other_url).await;
        assert!(loaded2.is_empty());

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn googlevideo_fingerprint_is_stable_across_signature_refresh() {
        // 同一视频同一清晰度：CDN 节点、签名、过期时间、客户端 IP 变化后指纹必须不变
        let a = url_path_fingerprint(
            "http://127.0.0.1:1234/videoplayback?id=o-abc123&itag=137&mh=1&mn=sn-a%2Csn-b&expire=111&sig=AAA&n=xyz&ip=1.2.3.4",
        );
        let b = url_path_fingerprint(
            "http://127.0.0.1:9999/videoplayback?id=o-abc123&itag=137&mh=1&mn=sn-b%2Csn-a&expire=222&sig=BBB&n=zzz&ip=5.6.7.8",
        );
        assert_eq!(a, b, "签名/节点刷新后 googlevideo 指纹应保持稳定");
        assert!(a.starts_with("googlevideo:"), "googlevideo 应使用内容指纹: {a}");

        // 清晰度变化（itag 不同）必须换指纹，否则续传会续到错误内容
        let c = url_path_fingerprint("http://127.0.0.1:1234/videoplayback?id=o-abc123&itag=136&mh=1&mn=sn-a%2Csn-b");
        assert_ne!(a, c, "不同 itag 必须得到不同指纹");

        // 非 googlevideo 仍走 path 指纹（不因 query 变化而失效）
        assert_eq!(
            url_path_fingerprint("https://cdn.example.com/v1/resource/a.m4s?deadline=1&sign=x"),
            "/v1/resource/a.m4s"
        );
    }

    /// 分片内断连续传：CDN 只发了一部分 body 就断开，续传请求必须从
    /// 「分片起点 + 已写字节」继续，而不是整片重下。
    #[tokio::test]
    async fn range_resumes_inside_segment_after_connection_drop() {
        let data: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let truncate_after = 1024 * 1024;
        let (url, requested) = spawn_truncating_range_server(data.clone(), truncate_after, "/res/test.m4s").await;
        let dir = std::env::temp_dir().join(format!("bili-resume-inside-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");
        let file = fs::File::create(&path).await.unwrap();
        file.set_len(data.len() as u64).await.unwrap();
        drop(file);

        let received = download_range_to_file(Client::new(), &url, &path, 0, data.len() as u64 - 1, None, None)
            .await
            .expect("分片内断流应自动续传成功");
        assert_eq!(received, data.len() as u64);
        assert_eq!(fs::read(&path).await.unwrap(), data, "续传后分片内容应与源逐字节一致");

        let reqs = requested.lock().unwrap().clone();
        assert!(reqs.len() >= 2, "应发生续传请求，实际: {reqs:?}");
        assert_eq!(reqs[0], (0, data.len() as u64 - 1));
        assert_eq!(
            reqs[1].0, truncate_after as u64,
            "续传起点应为已写字节位置，实际: {reqs:?}"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    /// 进程重启后（sidecar 已记录完成分片）googlevideo 只请求缺失分片。
    #[tokio::test]
    async fn googlevideo_restart_only_requests_missing_ranges() {
        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let url_path = "/videoplayback?id=o-restart&itag=137&mh=1&mn=sn-a%2Csn-b&expire=1&sig=x";
        let (url, requested, _fail_start, _fail_left) = spawn_range_test_server(data.clone(), None, url_path).await;
        let dir = std::env::temp_dir().join(format!("bili-resume-restart-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");
        let total = data.len() as u64;
        let (_, ranges) = build_parallel_ranges(total, 4, true, GOOGLEVIDEO_SEGMENT_SIZE, 16);
        assert!(ranges.len() >= 2, "测试数据应至少拆成两个分片");

        // 模拟上次进程已完整下载第一个分片后退出：写 sidecar + 落盘数据
        let sidecar = resume_sidecar_path(&path);
        ensure_resume_fingerprint(&sidecar, &url).await;
        append_completed_range(&sidecar, ranges[0].0, ranges[0].1).await;
        fs::write(&path, &data).await.unwrap();

        let downloader = Downloader::new(Client::new());
        downloader
            .fetch_parallel(&url, &path, 4, 16, None, None)
            .await
            .expect("重启后续传应成功");

        let reqs = requested.lock().unwrap().clone();
        assert!(!reqs.is_empty(), "重启后应请求缺失分片");
        assert!(
            reqs.iter().all(|(s, _)| *s > ranges[0].1),
            "重启后不应重下已完成分片，实际: {reqs:?}"
        );
        assert_eq!(fs::read(&path).await.unwrap(), data);

        let _ = fs::remove_dir_all(&dir).await;
    }

    /// googlevideo 完整路径（fetch_parallel_impl + is_googlevideo=true）：
    /// CDN 中途掐断后分片内续传、`.resume` 内容格式、以及直链刷新后指纹命中。
    #[tokio::test]
    async fn googlevideo_impl_resumes_after_midstream_drop() {
        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let truncate_after = 512 * 1024;
        let url_path = "/videoplayback?id=o-impl&itag=137&mh=1&mn=sn-a%2Csn-b&expire=1&sig=x";
        let (url, requested) = spawn_truncating_range_server(data.clone(), truncate_after, url_path).await;
        let dir = std::env::temp_dir().join(format!("bili-googlevideo-impl-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");

        let downloader = Downloader::new(Client::new());
        downloader
            .fetch_parallel_impl(&url, &path, 1, 16, None, None, true)
            .await
            .expect("googlevideo 分片级续传应成功");

        assert_eq!(fs::read(&path).await.unwrap(), data, "续传后文件内容应与源一致");
        let reqs = requested.lock().unwrap().clone();
        assert!(reqs.len() >= 3, "应发生分片内续传与第二个分片请求，实际: {reqs:?}");
        // YouTube 单文件连接数独立生效后，两个分片会并发下载，请求顺序不固定，按内容断言。
        let first_segment = (0, 4 * 1024 * 1024 - 1);
        let second_segment = (4 * 1024 * 1024, 8 * 1024 * 1024 - 1);
        assert!(reqs.contains(&first_segment), "应请求第一个 4MiB 分片，实际: {reqs:?}");
        assert!(reqs.contains(&second_segment), "应请求第二个 4MiB 分片，实际: {reqs:?}");
        assert!(
            reqs.contains(&(truncate_after as u64, 4 * 1024 * 1024 - 1)),
            "断流后应从已写字节继续下载第一个分片，实际: {reqs:?}"
        );

        let sidecar = resume_sidecar_path(&path);
        let content = fs::read_to_string(&sidecar)
            .await
            .expect("googlevideo 媒体流应保留 .resume 断点文件");
        let mut lines = content.lines();
        let fingerprint = lines.next().expect("首行应为内容指纹");
        assert!(
            fingerprint.starts_with("googlevideo:"),
            "googlevideo 指纹应以 googlevideo: 开头，实际: {fingerprint}"
        );
        assert!(
            fingerprint.contains("id=o-impl") && fingerprint.contains("itag=137"),
            "指纹应包含内容标识参数，实际: {fingerprint}"
        );
        assert!(fingerprint.contains("mh=1") && fingerprint.contains("mn=sn-a,sn-b"));
        // 两个分片并发下载，完成记录写入顺序不固定，按内容断言。
        let mut completed = Vec::new();
        for line in lines {
            completed.push(parse_range_line(line).expect("分片记录应为 start,end"));
        }
        assert!(
            completed.contains(&first_segment),
            "第一个分片应被标记完成，实际: {completed:?}"
        );
        assert!(
            completed.contains(&second_segment),
            "第二个分片也应被标记完成，实际: {completed:?}"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    /// googlevideo 完整路径：只有 sig/expire 刷新时指纹命中、不重下；itag 变化时
    /// 指纹失效、断点作废并从 0 重下。
    #[tokio::test]
    async fn googlevideo_impl_fingerprint_hit_and_itag_change_restart() {
        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let url_path = "/videoplayback?id=o-fp&itag=137&mh=1&mn=sn-a%2Csn-b&expire=1&sig=x";
        let (url, requested, _fail_start, _fail_left) = spawn_range_test_server(data.clone(), None, url_path).await;
        let dir = std::env::temp_dir().join(format!("bili-googlevideo-fp-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir).await;
        let path = dir.join("video.tmp_video");
        let downloader = Downloader::new(Client::new());

        downloader
            .fetch_parallel_impl(&url, &path, 2, 16, None, None, true)
            .await
            .expect("首次下载应成功");
        assert_eq!(fs::read(&path).await.unwrap(), data);
        let sidecar = resume_sidecar_path(&path);
        let first_fingerprint = fs::read_to_string(&sidecar)
            .await
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(first_fingerprint.starts_with("googlevideo:"));
        let requests_after_first = requested.lock().unwrap().len();

        // 只有 sig/expire 刷新、id/itag 不变 → 指纹命中，不应产生任何新 Range 请求
        let refreshed = url.replace("expire=1&sig=x", "expire=2&sig=y");
        downloader
            .fetch_parallel_impl(&refreshed, &path, 2, 16, None, None, true)
            .await
            .expect("直链刷新后应命中指纹并直接返回");
        let requests_after_refresh = requested.lock().unwrap().len();
        assert_eq!(
            requests_after_refresh, requests_after_first,
            "指纹命中时不应重新请求任何分片"
        );
        assert_eq!(
            fs::read_to_string(&sidecar).await.unwrap().lines().next().unwrap(),
            first_fingerprint,
            "直链刷新不应改变内容指纹"
        );

        // itag 变化 → 指纹不匹配，状态作废并从 0 重下
        let changed = url.replace("itag=137", "itag=136");
        downloader
            .fetch_parallel_impl(&changed, &path, 2, 16, None, None, true)
            .await
            .expect("itag 变化后应重新下载成功");
        let requests = requested.lock().unwrap().clone();
        assert!(
            requests[requests_after_refresh..].iter().any(|(s, _)| *s == 0),
            "itag 变化后应从 0 重新下载，实际: {:?}",
            &requests[requests_after_refresh..]
        );
        assert_eq!(fs::read(&path).await.unwrap(), data);
        let new_fingerprint = fs::read_to_string(&sidecar)
            .await
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(
            new_fingerprint.contains("itag=136"),
            "断点文件指纹应更新为新 itag，实际: {new_fingerprint}"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }
}
