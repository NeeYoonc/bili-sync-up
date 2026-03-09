use anyhow::{bail, Context, Result};
use axum::extract::Path;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, error, info, warn};

use bili_sync_entity::entities::{page, video};

/// Range请求参数
#[derive(Debug)]
pub struct RangeSpec {
    pub start: u64,
    pub end: Option<u64>,
}

/// 解析HTTP Range头
pub fn parse_range_header(range_header: &str, file_size: u64) -> Result<RangeSpec> {
    if file_size == 0 {
        bail!("Empty file cannot satisfy range requests");
    }

    if !range_header.starts_with("bytes=") {
        bail!("Invalid range header format");
    }

    let range_part = &range_header[6..]; // 去掉 "bytes="
    let parts: Vec<&str> = range_part.split('-').collect();

    if parts.len() != 2 {
        bail!("Invalid range format");
    }

    let start = if parts[0].is_empty() {
        // 如果start为空，表示要最后N字节
        if let Ok(suffix_length) = parts[1].parse::<u64>() {
            file_size.saturating_sub(suffix_length)
        } else {
            bail!("Invalid suffix range");
        }
    } else {
        parts[0].parse::<u64>().context("Invalid start position")?
    };

    let end = if parts[1].is_empty() {
        None // 到文件结尾
    } else {
        Some(parts[1].parse::<u64>().context("Invalid end position")?)
    };

    // 验证范围
    let actual_end = end.unwrap_or(file_size - 1).min(file_size - 1);
    if start > actual_end {
        bail!("Invalid range: start > end");
    }

    Ok(RangeSpec {
        start,
        end: Some(actual_end),
    })
}

/// 流式传输视频文件
pub async fn stream_video(
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Extension(db): Extension<Arc<DatabaseConnection>>,
) -> impl IntoResponse {
    match stream_video_impl(video_id, headers, db).await {
        Ok(response) => response,
        Err(e) => {
            error!("视频流传输失败: {:#}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

async fn stream_video_impl(video_id: String, headers: HeaderMap, db: Arc<DatabaseConnection>) -> Result<Response> {
    debug!("请求视频流: {}", video_id);

    // 从数据库查询视频文件路径
    let video_path = find_video_file(&video_id, &db).await?;

    // 兼容历史文件：有些老视频只提供 FLV 混合流，若被保存为 .mp4 会导致网页端无法播放
    // 这里在播放前进行一次轻量自愈转封装，成功后会原地替换为可播放的 mp4
    maybe_remux_flv_in_mp4(&video_path).await?;

    let file_size = fs::metadata(&video_path).await.context("无法获取文件大小")?.len();

    debug!("视频文件路径: {:?}, 大小: {} bytes", video_path, file_size);

    if file_size == 0 {
        info!("检测到充电视频占位文件，返回不可播放提示: {:?}", video_path);
        let mut response = Response::new("充电视频未充电".into());
        *response.status_mut() = StatusCode::FORBIDDEN;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return Ok(response);
    }

    // 检查是否有Range请求
    if let Some(range_header) = headers.get(header::RANGE) {
        let range_str = range_header.to_str().context("Invalid range header")?;
        debug!("Range请求: {}", range_str);

        match parse_range_header(range_str, file_size) {
            Ok(range) => serve_partial_content(&video_path, range, file_size).await,
            Err(e) => {
                warn!("Range解析失败: {:#}, 返回完整文件", e);
                serve_full_content(&video_path, file_size).await
            }
        }
    } else {
        debug!("完整文件请求");
        serve_full_content(&video_path, file_size).await
    }
}

fn file_starts_with_flv(video_path: &PathBuf) -> Result<bool> {
    let mut file = File::open(video_path).with_context(|| format!("无法打开视频文件: {:?}", video_path))?;
    let mut header = [0u8; 3];
    let read = file.read(&mut header).context("读取文件头失败")?;
    Ok(read == 3 && header == *b"FLV")
}

async fn maybe_remux_flv_in_mp4(video_path: &PathBuf) -> Result<()> {
    let is_mp4_ext = video_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
    if !is_mp4_ext {
        return Ok(());
    }

    if !file_starts_with_flv(video_path)? {
        return Ok(());
    }

    let stem = video_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string());

    let tmp_output = video_path.with_file_name(format!("{}.remux_tmp.mp4", stem));
    let backup = video_path.with_file_name(format!("{}.flv_source.bak", stem));

    info!("检测到 FLV 内容被保存为 .mp4，尝试转封装修复: {:?}", video_path);

    let _ = fs::remove_file(&tmp_output).await;
    crate::downloader::remux_with_ffmpeg(video_path, &tmp_output)
        .await
        .with_context(|| "FLV 转封装失败，请确认已安装并可执行 ffmpeg")?;

    let _ = fs::remove_file(&backup).await;

    // 先备份原文件，再原地替换；失败时尽量不影响继续访问原文件
    if let Err(e) = fs::rename(video_path, &backup).await {
        warn!("转封装替换前备份原文件失败，将跳过替换: {:#}", e);
        let _ = fs::remove_file(&tmp_output).await;
        return Ok(());
    }

    if let Err(e) = fs::rename(&tmp_output, video_path).await {
        warn!("转封装替换失败，尝试还原原文件: {:#}", e);
        let _ = fs::rename(&backup, video_path).await;
        let _ = fs::remove_file(&tmp_output).await;
        return Ok(());
    }

    let _ = fs::remove_file(&backup).await;
    Ok(())
}

/// 查找视频文件路径
async fn find_video_file(video_id: &str, db: &DatabaseConnection) -> Result<PathBuf> {
    debug!("查找视频文件: {}", video_id);

    // 首先尝试作为分页ID查找
    if let Ok(page_id) = video_id.parse::<i32>() {
        // 尝试从page表查找
        if let Some(page_record) = page::Entity::find_by_id(page_id)
            .one(db)
            .await
            .context("查询分页记录失败")?
        {
            if let Some(file_path) = &page_record.path {
                let page_path = PathBuf::from(file_path);
                if page_path.exists() && page_path.is_file() {
                    debug!("通过分页ID找到视频文件: {:?}", page_path);
                    return Ok(page_path);
                }
            }

            // 如果分页没有路径或路径无效，返回错误
            bail!("分页记录存在但没有有效的文件路径: page_id={}", page_id);
        }
    }

    // 尝试解析video_id为数字ID或BVID
    let video_model = if let Ok(id) = video_id.parse::<i32>() {
        // 按数字ID查找
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

    // 检查下载状态 - download_status的第2位(索引1)为2表示已完成
    // 这里我们简化检查，只要文件存在就可以播放

    // 获取文件路径
    let video_path = PathBuf::from(&video.path);

    // 检查路径是否存在
    if !video_path.exists() {
        bail!("视频路径不存在: {:?}", video_path);
    }

    // 如果是文件夹，在其中查找视频文件
    let actual_video_file = if video_path.is_dir() {
        info!("检测到文件夹路径，开始查找视频文件: {:?}", video_path);
        find_video_file_in_directory(&video_path).await?
    } else {
        // 如果是文件，直接使用
        video_path
    };

    // 如果仍然找不到，尝试查找Pages表中的视频文件
    if !actual_video_file.exists() || actual_video_file.is_dir() {
        info!("主路径未找到视频文件，尝试查找Pages表");
        let pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(video.id))
            .all(db)
            .await
            .context("查询视频页面失败")?;

        for page_record in pages {
            if let Some(file_path) = &page_record.path {
                let page_path = PathBuf::from(file_path);
                if page_path.exists() && page_path.is_file() {
                    info!("找到页面文件: {:?}", page_path);

                    // 检查文件权限和可读性
                    match std::fs::metadata(&page_path) {
                        Ok(metadata) => {
                            info!(
                                "页面文件元数据 - 大小: {} bytes, 只读: {}",
                                metadata.len(),
                                metadata.permissions().readonly()
                            );
                            return Ok(page_path);
                        }
                        Err(e) => {
                            warn!("无法获取页面文件元数据: {}", e);
                            continue;
                        }
                    }
                }
            }
        }

        bail!("未找到可播放的视频文件");
    }

    debug!("找到视频文件: {:?}", actual_video_file);

    // 检查文件权限和可读性
    match std::fs::metadata(&actual_video_file) {
        Ok(metadata) => {
            info!(
                "文件元数据 - 大小: {} bytes, 只读: {}",
                metadata.len(),
                metadata.permissions().readonly()
            );
        }
        Err(e) => {
            error!("无法获取文件元数据: {}", e);
            bail!("文件元数据获取失败: {:?}", e);
        }
    }

    Ok(actual_video_file)
}

/// 在指定文件夹中查找视频文件
async fn find_video_file_in_directory(dir_path: &PathBuf) -> Result<PathBuf> {
    debug!("在文件夹中查找视频文件: {:?}", dir_path);

    let video_extensions = ["mp4", "mkv", "avi", "webm", "flv", "mov", "wmv", "m4v"];

    // 读取文件夹内容
    let mut entries = fs::read_dir(dir_path).await.context("无法读取文件夹内容")?;

    while let Some(entry) = entries.next_entry().await.context("读取文件夹条目失败")? {
        let path = entry.path();

        if path.is_file() {
            if let Some(extension) = path.extension() {
                if let Some(ext_str) = extension.to_str() {
                    if video_extensions.contains(&ext_str.to_lowercase().as_str()) {
                        info!("找到视频文件: {:?}", path);
                        return Ok(path);
                    }
                }
            }
        }
    }

    bail!("文件夹中未找到视频文件: {:?}", dir_path);
}

/// 根据文件扩展名获取MIME类型
fn get_video_mime_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        Some("flv") => "video/x-flv",
        Some("wmv") => "video/x-ms-wmv",
        Some("m4v") => "video/x-m4v",
        _ => "video/mp4", // 默认
    }
}

/// 提供部分内容 (Range请求)
async fn serve_partial_content(video_path: &PathBuf, range: RangeSpec, file_size: u64) -> Result<Response> {
    let start = range.start;
    let end = range.end.unwrap_or(file_size - 1);
    let content_length = end - start + 1;

    debug!("Range请求: {}-{}/{} ({} bytes)", start, end, file_size, content_length);

    // 限制单次请求的最大大小为10MB
    const MAX_CHUNK_SIZE: u64 = 10 * 1024 * 1024;
    let actual_end = if content_length > MAX_CHUNK_SIZE {
        start + MAX_CHUNK_SIZE - 1
    } else {
        end
    };
    let actual_content_length = actual_end - start + 1;

    // 读取文件片段
    debug!("尝试打开文件: {:?}", video_path);
    let mut file = File::open(video_path).with_context(|| format!("无法打开视频文件: {:?}", video_path))?;
    file.seek(SeekFrom::Start(start)).context("文件定位失败")?;

    let mut buffer = vec![0u8; actual_content_length as usize];
    file.read_exact(&mut buffer).context("读取文件内容失败")?;

    // 构建响应头
    let mut response = Response::new(buffer.into());
    *response.status_mut() = StatusCode::PARTIAL_CONTENT;

    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(get_video_mime_type(video_path)),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&actual_content_length.to_string())?,
    );
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {}-{}/{}", start, actual_end, file_size))?,
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));

    // 添加缓存控制头，让浏览器更好地处理视频流
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));

    Ok(response)
}

/// 提供完整内容
async fn serve_full_content(video_path: &PathBuf, file_size: u64) -> Result<Response> {
    debug!("提供完整文件: {} bytes", file_size);

    // 对于大文件，返回前10MB的内容，让浏览器使用Range请求获取后续内容
    const INITIAL_CHUNK_SIZE: u64 = 10 * 1024 * 1024;
    let content_length = file_size.min(INITIAL_CHUNK_SIZE);

    info!("读取文件前 {} bytes: {:?}", content_length, video_path);
    let mut file = File::open(video_path).with_context(|| format!("无法打开视频文件: {:?}", video_path))?;
    let mut buffer = vec![0u8; content_length as usize];
    file.read_exact(&mut buffer).context("读取文件内容失败")?;

    let mut response = Response::new(buffer.into());

    if content_length < file_size {
        // 如果只返回部分内容，使用206状态码
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes 0-{}/{}", content_length - 1, file_size))?,
        );
    } else {
        // 如果返回完整内容，使用200状态码
        *response.status_mut() = StatusCode::OK;
    }

    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(get_video_mime_type(video_path)),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string())?,
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));

    Ok(response)
}
