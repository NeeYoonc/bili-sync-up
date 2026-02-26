use core::str;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use futures::TryStreamExt;
use reqwest::{header, Method, StatusCode};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::StreamReader;
use tracing::{debug, error, info, warn};

use crate::bilibili::Client;
pub struct Downloader {
    client: Client,
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
        let config = crate::config::reload_config();
        let parallel = &config.concurrent_limit.parallel_download;

        if parallel.enabled && parallel.threads > 1 {
            match self.fetch_parallel(url, path, parallel.threads).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    debug!("原生多线程下载不可用，回退到单线程下载: {:#}", e);
                }
            }
        }

        self.fetch_single(url, path).await
    }

    async fn fetch_single(&self, url: &str, path: &Path) -> Result<()> {
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

        let resp = match self.client.request(Method::GET, url, None).send().await {
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
        };

        let expected = resp.header_content_length().unwrap_or_default();

        let mut stream_reader = StreamReader::new(resp.bytes_stream().map_err(std::io::Error::other));
        let received = match tokio::io::copy(&mut stream_reader, &mut file).await {
            Ok(size) => size,
            Err(e) => {
                error!("下载过程中出错: {:#}", e);
                return Err(e.into());
            }
        };

        file.flush().await?;

        ensure!(
            received >= expected,
            "received {} bytes, expected {} bytes",
            received,
            expected
        );

        Ok(())
    }

    async fn fetch_parallel(&self, url: &str, path: &Path, threads: usize) -> Result<()> {
        const MIN_PARALLEL_SIZE: u64 = 4 * 1024 * 1024; // 4MB 以下不分片，避免小文件开销
        const MIN_SEGMENT_SIZE: u64 = 1 * 1024 * 1024; // 每片至少 1MB，避免过多分片

        // 创建父目录
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).await?;
            }
        }

        let (total_size, range_supported) = self.get_size_and_range_support(url).await?;
        ensure!(total_size > 0, "无法获取文件大小");
        ensure!(
            total_size >= MIN_PARALLEL_SIZE,
            "文件过小({} bytes)，不启用分片下载",
            total_size
        );
        ensure!(range_supported, "服务器不支持Range分片下载");

        // 计算分片数（按最小分片大小限制）
        let max_segments = ((total_size + MIN_SEGMENT_SIZE - 1) / MIN_SEGMENT_SIZE) as usize;
        let segment_count = threads.min(max_segments).max(1);
        ensure!(segment_count > 1, "分片数不足，跳过多线程下载");

        let total_mb = total_size as f64 / 1024.0 / 1024.0;
        info!(
            "原生多线程下载启用: 大小={:.2}MB, 分片数={}, 线程数={}",
            total_mb, segment_count, threads
        );

        // 预创建并设置目标文件大小，便于随机写入
        {
            let file = File::create(path).await?;
            file.set_len(total_size).await?;
        }

        let url_owned = url.to_string();
        let path_owned = path.to_path_buf();
        let mut tasks = Vec::with_capacity(segment_count);

        let base = total_size / segment_count as u64;
        let mut start = 0u64;
        for i in 0..segment_count {
            let end = if i == segment_count - 1 {
                total_size - 1
            } else {
                start + base - 1
            };

            let client = self.client.clone();
            let url = url_owned.clone();
            let path = path_owned.clone();
            let part_start = start;
            let part_end = end;

            tasks.push(async move { download_range_to_file(client, &url, &path, part_start, part_end).await });

            start = end + 1;
        }

        let results = futures::future::try_join_all(tasks).await?;
        let downloaded: u64 = results.into_iter().sum();
        ensure!(
            downloaded == total_size,
            "分片下载大小不一致: {} != {}",
            downloaded,
            total_size
        );

        Ok(())
    }

    async fn get_size_and_range_support(&self, url: &str) -> Result<(u64, bool)> {
        let mut total_size = None;
        let mut range_supported = false;

        let head_resp = self
            .client
            .request(Method::HEAD, url, None)
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await;

        if let Ok(resp) = head_resp {
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
            let (probe_supported, probe_size) = self.probe_range_support_and_size(url).await?;
            range_supported = range_supported || probe_supported;
            if total_size.is_none() {
                total_size = probe_size.filter(|size| *size > 0);
            }
        }

        Ok((total_size.unwrap_or(0), range_supported))
    }

    async fn probe_range_support_and_size(&self, url: &str) -> Result<(bool, Option<u64>)> {
        let resp = self
            .client
            .request(Method::GET, url, None)
            .header(header::RANGE, "bytes=0-0")
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
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
        if urls.is_empty() {
            bail!("no urls provided");
        }

        let mut last_error = None;
        for url in urls.iter() {
            match self.fetch(url, path).await {
                Ok(_) => {
                    return Ok(());
                }
                Err(err) => {
                    warn!("下载失败: {:#}", err);
                    last_error = Some(err);
                }
            }
        }

        error!("所有URL尝试失败");
        match last_error {
            Some(err) => Err(err).with_context(|| format!("failed to download from {:?}", urls)),
            None => bail!("no urls to try"),
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

async fn download_range_to_file(client: Client, url: &str, path: &Path, start: u64, end: u64) -> Result<u64> {
    let expected = end.saturating_sub(start) + 1;

    let mut file = OpenOptions::new().write(true).open(path).await?;
    file.seek(std::io::SeekFrom::Start(start)).await?;

    let range_value = format!("bytes={}-{}", start, end);
    let resp = client
        .request(Method::GET, url, None)
        .header(header::RANGE, range_value)
        .header(header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .context("Range下载请求失败")?;

    ensure!(
        resp.status() == StatusCode::PARTIAL_CONTENT,
        "Range响应异常: {}",
        resp.status()
    );

    let resp = resp.error_for_status().context("Range状态码错误")?;

    let mut stream_reader = StreamReader::new(resp.bytes_stream().map_err(std::io::Error::other));
    let received = tokio::io::copy(&mut stream_reader, &mut file).await?;
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
