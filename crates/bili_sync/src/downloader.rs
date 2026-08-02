use core::str;
use std::collections::HashMap;
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
// 分片并复用有限数量的连接槽，可以继续走项目原生下载器，同时避免一个
// 50~500MiB 大分片在限速连接上持续数十分钟。
const GOOGLEVIDEO_SEGMENT_SIZE: u64 = 4 * 1024 * 1024;
const GOOGLEVIDEO_MAX_CONNECTIONS: usize = 4;
const RANGE_DOWNLOAD_ATTEMPTS: usize = 3;
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

static BAD_CDN_HOSTS: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn url_host(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
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

        if parallel.enabled && parallel.threads > 1 {
            match self.fetch_parallel(url, path, parallel.threads, referer, None).await {
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
                            "资源「{}」原生多线程下载失败，改用原生单连接下载",
                            path.display()
                        );
                    }
                }
            }
        }

        let result = self.fetch_single(url, path, referer, None).await;
        if result.is_err() {
            let _ = fs::remove_file(path).await;
        }
        result
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
                        error!("HTTP状态码错误: {:#}", e);
                        return Err(e.into());
                    }
                },
                Err(e) => {
                    error!("HTTP请求失败: {:#}", e);
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
                    error!("下载过程中出错: {:#}", error);
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
        referer: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<()> {
        let is_googlevideo = url_host(url).is_some_and(|host| host.ends_with(".googlevideo.com"));
        // 小型 GoogleVideo 单连接也可能在完整传输前直接关闭 TLS；从 1MiB 起走
        // Range 分片可让失败只影响一个小分片。
        let min_parallel_size = if is_googlevideo {
            MIN_SEGMENT_SIZE
        } else {
            MIN_PARALLEL_SIZE
        };

        // 创建父目录
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }

        let (total_size, range_supported) = self.get_size_and_range_support(url, referer, cookie).await?;
        ensure!(total_size > 0, "无法获取文件大小");
        ensure!(
            total_size >= min_parallel_size,
            "文件过小({} bytes)，不启用分片下载",
            total_size
        );
        ensure!(range_supported, "服务器不支持Range分片下载");

        let (concurrency, ranges) = build_parallel_ranges(total_size, threads, is_googlevideo);
        ensure!(ranges.len() > 1, "分片数不足，跳过多线程下载");

        let total_mb = total_size as f64 / 1024.0 / 1024.0;
        info!(
            "原生多线程下载启用: 文件「{}」, 大小={:.2}MB, 分片数={}, 并发连接={}",
            path.display(),
            total_mb,
            ranges.len(),
            concurrency
        );

        // 预创建并设置目标文件大小，便于随机写入
        {
            let file = File::create(path).await?;
            file.set_len(total_size).await?;
        }

        let url_owned = url.to_string();
        let path_owned = path.to_path_buf();
        let referer_owned = referer.map(str::to_string);
        let cookie_owned = cookie.map(str::to_string);
        let tasks = futures::stream::iter(ranges.into_iter().map(|(part_start, part_end)| {
            let client = self.client.clone();
            let url = url_owned.clone();
            let path = path_owned.clone();
            let referer = referer_owned.clone();
            let cookie = cookie_owned.clone();
            async move {
                download_range_to_file_with_retry(
                    client,
                    &url,
                    &path,
                    part_start,
                    part_end,
                    RANGE_DOWNLOAD_ATTEMPTS,
                    referer.as_deref(),
                    cookie.as_deref(),
                )
                .await
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
        let mut downloaded = 0u64;
        for result in tasks {
            downloaded = downloaded.saturating_add(result?);
        }
        ensure!(
            downloaded == total_size,
            "分片下载大小不一致: {} != {}",
            downloaded,
            total_size
        );

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
            let result = if parallel.enabled && parallel.threads > 1 {
                match self
                    .fetch_parallel(url, path, parallel.threads, Some(referer), Some(cookie))
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
                            "Cookie 媒体「{}」无法进行 Range 分片，改用原生单连接下载",
                            path.display()
                        );
                        self.fetch_single(url, path, Some(referer), Some(cookie)).await
                    }
                }
            } else {
                self.fetch_single(url, path, Some(referer), Some(cookie)).await
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let _ = fs::remove_file(path).await;
                    warn!("下载资源「{}」失败: {error:#}", path.display());
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
            if is_url_blocked_by_bad_cdn_host(url) {
                debug!("跳过短期内已判定证书异常的 CDN URL: {}", url);
                continue;
            }

            let result = match referer {
                Some(referer) => self.fetch_with_referer(url, path, referer).await,
                None => self.fetch(url, path).await,
            };
            match result {
                Ok(_) => {
                    return Ok(());
                }
                Err(err) => {
                    if is_certificate_name_mismatch_error(&err) {
                        mark_bad_cdn_host(url, &err);
                    }
                    warn!("下载资源「{}」失败: {:#}", path.display(), err);
                    last_error = Some(err);
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

        let output = tokio::process::Command::new(resolve_media_tool_path("ffmpeg"))
            .args(args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = str::from_utf8(&output.stderr).unwrap_or("unknown");
            error!("FFmpeg错误: {}", stderr);
            bail!("ffmpeg error: {}", stderr);
        }

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
        let result = tokio::process::Command::new(resolve_media_tool_path("ffprobe"))
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

fn build_parallel_ranges(total_size: u64, threads: usize, is_googlevideo: bool) -> (usize, Vec<(u64, u64)>) {
    if total_size == 0 {
        return (1, Vec::new());
    }

    let concurrency = if is_googlevideo {
        threads.min(GOOGLEVIDEO_MAX_CONNECTIONS)
    } else {
        threads
    }
    .max(1);

    if is_googlevideo {
        let mut ranges = Vec::new();
        let mut start = 0u64;
        while start < total_size {
            let end = start.saturating_add(GOOGLEVIDEO_SEGMENT_SIZE - 1).min(total_size - 1);
            ranges.push((start, end));
            start = end.saturating_add(1);
        }
        return (concurrency, ranges);
    }

    let max_segments = ((total_size + MIN_SEGMENT_SIZE - 1) / MIN_SEGMENT_SIZE) as usize;
    let segment_count = concurrency.min(max_segments).max(1);
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

async fn download_range_to_file_with_retry(
    client: Client,
    url: &str,
    path: &Path,
    start: u64,
    end: u64,
    attempts: usize,
    referer: Option<&str>,
    cookie: Option<&str>,
) -> Result<u64> {
    let mut last_error = None;
    for attempt in 1..=attempts.max(1) {
        match download_range_to_file(client.clone(), url, path, start, end, referer, cookie).await {
            Ok(downloaded) => return Ok(downloaded),
            Err(error) if is_certificate_name_mismatch_error(&error) => return Err(error),
            Err(error) => {
                warn!(
                    start,
                    end,
                    attempt,
                    attempts,
                    error = %error,
                    "Range 分片下载失败，准备重试当前分片"
                );
                last_error = Some(error);
                if attempt < attempts {
                    tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
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
    file.seek(std::io::SeekFrom::Start(start)).await?;

    let range_value = format!("bytes={}-{}", start, end);
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

    let mut received = 0u64;
    let mut stream = resp.bytes_stream();
    let mut first_chunk = true;
    loop {
        let wait = if first_chunk {
            FIRST_BYTE_TIMEOUT
        } else {
            CHUNK_IDLE_TIMEOUT
        };
        let next = tokio::time::timeout(wait, stream.next())
            .await
            .map_err(|_| anyhow!("Range 分片 bytes={start}-{end} 等待数据超时（{} 秒）", wait.as_secs()))?;
        let Some(chunk) = next else { break };
        first_chunk = false;
        match chunk {
            Ok(chunk) => {
                file.write_all(&chunk).await?;
                received += chunk.len() as u64;
            }
            Err(error) if received >= expected && contains_tls_close_notify_eof(&error.to_string()) => {
                warn!(
                    "Range CDN 未发送 TLS close_notify，但分片已完整接收: received={} expected={}",
                    received, expected
                );
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
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

    let output = tokio::process::Command::new(resolve_media_tool_path("ffmpeg"))
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
    let output = match tokio::process::Command::new(resolve_media_tool_path("ffprobe"))
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

    let mut output = tokio::process::Command::new(resolve_media_tool_path("ffmpeg"))
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
            output = tokio::process::Command::new(resolve_media_tool_path("ffmpeg"))
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

    let output = tokio::process::Command::new(resolve_media_tool_path("ffmpeg"))
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
    fn googlevideo_uses_small_ranges_with_bounded_native_concurrency() {
        let total_size = 18 * 1024 * 1024;
        let (concurrency, ranges) = build_parallel_ranges(total_size, 16, true);

        assert_eq!(concurrency, GOOGLEVIDEO_MAX_CONNECTIONS);
        assert_eq!(ranges.len(), 5);
        assert_eq!(ranges[0], (0, GOOGLEVIDEO_SEGMENT_SIZE - 1));
        assert_eq!(ranges[3], (12 * 1024 * 1024, 16 * 1024 * 1024 - 1));
        assert_eq!(ranges[4], (16 * 1024 * 1024, total_size - 1));
    }

    #[test]
    fn regular_cdn_keeps_configured_parallel_range_count() {
        let total_size = 64 * 1024 * 1024;
        let (concurrency, ranges) = build_parallel_ranges(total_size, 8, false);

        assert_eq!(concurrency, 8);
        assert_eq!(ranges.len(), 8);
        assert_eq!(ranges[0], (0, 8 * 1024 * 1024 - 1));
        assert_eq!(ranges[7], (56 * 1024 * 1024, total_size - 1));
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
}
