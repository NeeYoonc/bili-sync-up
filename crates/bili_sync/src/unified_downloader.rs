use anyhow::Result;
use std::path::Path;
use tracing::{info, warn};

use crate::aria2_downloader::Aria2Downloader;
use crate::bilibili::Client;
use crate::downloader::Downloader;

/// 统一下载器，可以在原生下载器和aria2下载器之间切换
pub enum UnifiedDownloader {
    Native(Downloader),
    Aria2(Aria2Downloader),
}

impl UnifiedDownloader {
    /// 创建原生下载器
    pub fn new_native(client: Client) -> Self {
        Self::Native(Downloader::new(client))
    }

    /// 创建aria2下载器
    pub async fn new_aria2(client: Client) -> Result<Self> {
        let aria2_downloader = Aria2Downloader::new(client).await?;
        Ok(Self::Aria2(aria2_downloader))
    }

    /// 智能创建下载器：根据配置决定使用哪种下载器
    pub async fn new_smart(client: Client) -> Self {
        // 获取最新配置
        let config = crate::config::reload_config();
        let parallel = &config.concurrent_limit.parallel_download;

        // 检查是否启用了多线程下载
        if !parallel.enabled {
            info!("多线程下载已禁用，使用原生下载器");
            return Self::new_native(client);
        }

        // 如果用户关闭了 aria2，则直接使用原生多线程分片下载
        if !parallel.use_aria2 {
            info!("已关闭aria2，使用原生多线程下载");
            return Self::new_native(client);
        }

        // 如果启用了多线程下载，尝试使用aria2
        match Self::new_aria2(client.clone()).await {
            Ok(downloader) => {
                info!("成功初始化aria2下载器");
                downloader
            }
            Err(e) => {
                warn!("aria2下载器初始化失败，回退到原生下载器: {:#}", e);
                Self::new_native(client)
            }
        }
    }

    /// 下载文件，支持多个URL备选
    pub async fn fetch_with_fallback(&self, urls: &[&str], path: &Path) -> Result<()> {
        match self {
            Self::Native(downloader) => downloader.fetch_with_fallback(urls, path).await,
            Self::Aria2(downloader) => downloader.fetch_with_aria2_fallback(urls, path).await,
        }
    }

    /// 沿用当前统一下载器，仅为本次下载任务应用代理。
    pub async fn fetch_with_fallback_with_proxy(&self, urls: &[&str], path: &Path, proxy: &str) -> Result<()> {
        let proxy = proxy.trim();
        if proxy.is_empty() {
            return self.fetch_with_fallback(urls, path).await;
        }
        match self {
            Self::Native(downloader) => downloader.fetch_with_fallback_with_proxy(urls, path, proxy).await,
            Self::Aria2(downloader) => downloader.fetch_with_aria2_fallback_with_proxy(urls, path, proxy).await,
        }
    }

    /// 下载文件并覆盖平台 Referer，原生与 aria2 路径保持一致。
    pub async fn fetch_with_fallback_with_referer(&self, urls: &[&str], path: &Path, referer: &str) -> Result<()> {
        match self {
            Self::Native(downloader) => downloader.fetch_with_fallback_with_referer(urls, path, referer).await,
            Self::Aria2(downloader) => {
                downloader
                    .fetch_with_aria2_fallback_with_referer(urls, path, referer)
                    .await
            }
        }
    }

    /// 下载同时要求平台 Referer、网页会话 Cookie 和任务代理的媒体（TikTok 等）。
    pub async fn fetch_with_fallback_with_referer_and_cookie_and_proxy(
        &self,
        urls: &[&str],
        path: &Path,
        referer: &str,
        cookie: &str,
        proxy: &str,
    ) -> Result<()> {
        match self {
            Self::Native(downloader) => {
                downloader
                    .fetch_with_fallback_with_referer_and_cookie_and_proxy(urls, path, referer, cookie, proxy)
                    .await
            }
            Self::Aria2(downloader) => {
                downloader
                    .fetch_with_aria2_fallback_with_referer_and_cookie_and_proxy(urls, path, referer, cookie, proxy)
                    .await
            }
        }
    }

    /// 下载同时要求平台 Referer 和网页会话 Cookie 的媒体。
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
        match self {
            Self::Native(downloader) => {
                downloader
                    .fetch_with_fallback_with_referer_and_cookie(urls, path, referer, cookie)
                    .await
            }
            Self::Aria2(downloader) => {
                downloader
                    .fetch_with_aria2_fallback_with_referer_and_cookie(urls, path, referer, cookie)
                    .await
            }
        }
    }

    /// 合并视频和音频文件
    pub async fn merge(&self, video_path: &Path, audio_path: &Path, output_path: &Path) -> Result<()> {
        match self {
            Self::Native(downloader) => downloader.merge(video_path, audio_path, output_path).await,
            Self::Aria2(downloader) => downloader.merge(video_path, audio_path, output_path).await,
        }
    }

    /// 优雅关闭下载器
    pub async fn shutdown(&self) -> Result<()> {
        match self {
            Self::Native(_) => {
                // 原生下载器不需要特殊关闭操作
                Ok(())
            }
            Self::Aria2(downloader) => downloader.shutdown().await,
        }
    }
}
