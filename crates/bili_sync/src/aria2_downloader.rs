use anyhow::{bail, Context, Result};
use reqwest::Client as ReqwestClient;
use reqwest::Method;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};
use zip::ZipArchive;

use crate::bilibili::Client;
use crate::config::CONFIG_DIR;
use crate::http::headers::{create_api_headers, create_aria2_headers, create_aria2_headers_with_referer};

/// aria2 运行时下载配置
const ARIA2_VERSION: &str = "1.37.0";
const ARIA2_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// 镜像目录环境变量：非空时只从该目录下载 aria2 压缩包（目录下需放同名压缩包），优先级最高。
const ARIA2_MIRROR_BASE_ENV: &str = "BILI_SYNC_ARIA2_MIRROR_BASE";

/// 归一化下载基址：去首尾空白、去尾部斜杠；空白或纯斜杠都视为未配置。
fn normalize_download_base(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// 读取 aria2 镜像基址；未配置或配置为空时返回 None。
fn aria2_mirror_base() -> Option<String> {
    normalize_download_base(std::env::var(ARIA2_MIRROR_BASE_ENV).ok().as_deref())
}

/// 计算 aria2 压缩包的实际下载地址：
/// - 配置了镜像基址（非空）时使用 `<base>/<archive_name>`，基址尾部斜杠会被去掉；
/// - 否则回退到给定的默认下载地址。
fn resolve_aria2_download_url(default_download_url: &str, archive_name: &str, mirror_base: Option<&str>) -> String {
    match normalize_download_base(mirror_base) {
        Some(base) => format!("{}/{}", base, archive_name),
        None => default_download_url.to_string(),
    }
}

/// aria2 压缩包的一个下载源。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Aria2DownloadSource {
    /// 日志里展示的源名称。
    label: &'static str,
    /// 实际下载地址（镜像源会拼接 archive_name）。
    url: String,
    /// 该源对应压缩包的 sha256（主源与备用源是同一份文件，值相同）。
    sha256: &'static str,
}

/// 计算本次实际尝试的下载源顺序：
/// - 配置 `BILI_SYNC_ARIA2_MIRROR_BASE`：只走镜像（单源、不回退）；
/// - 否则：主源 `r2d.dnof.net` 公开分享直链优先，失败（网络 / 非 2xx / 校验失败）回退本仓库
///   `tools-aria2` Release 链接。
fn plan_aria2_download_sources(package: &Aria2Package, mirror_base: Option<&str>) -> Vec<Aria2DownloadSource> {
    if let Some(base) = normalize_download_base(mirror_base) {
        return vec![Aria2DownloadSource {
            label: "镜像",
            url: resolve_aria2_download_url(package.primary_url, package.archive_name, Some(&base)),
            sha256: package.sha256,
        }];
    }
    vec![
        Aria2DownloadSource {
            label: "公开镜像(r2d.dnof.net)",
            url: package.primary_url.to_string(),
            sha256: package.sha256,
        },
        Aria2DownloadSource {
            label: "GitHub Release(备用源)",
            url: package.fallback_url.to_string(),
            sha256: package.sha256,
        },
    ]
}

/// 当前源失败后应尝试的下一个源；没有则为 None（所有源都试过了）。
fn next_aria2_source(sources: &[Aria2DownloadSource], failed_index: usize) -> Option<&Aria2DownloadSource> {
    sources.get(failed_index + 1)
}

/// 所有下载源都失败时的错误文案：逐个列出源名称与失败原因，并附手动放置 / 镜像提示。
fn aria2_download_failure_message(failures: &[(String, String)], manual_hint: &str) -> String {
    let detail = failures
        .iter()
        .map(|(label, error)| format!("{label}: {error}"))
        .collect::<Vec<_>>()
        .join("；");
    format!(
        "下载 aria2 包失败（已尝试 {} 个源）；{detail}；{manual_hint}",
        failures.len()
    )
}

static ARIA2_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// aria2 RPC 端点（端口 + token），用于把「往哪个实例提交任务」收成一个参数。
struct Aria2Rpc<'a> {
    port: u16,
    secret: &'a str,
}

/// 下载请求附带的 HTTP 头线索，打包成一个参数，避免参数列表过长。
struct RequestHints<'a> {
    referer: Option<&'a str>,
    cookie: Option<&'a str>,
    proxy: Option<&'a str>,
}

/// aria2 运行时包描述：主源（r2d 公开分享直链）与备用源（本仓库 Release）指向同一个文件。
#[derive(Clone, Copy)]
struct Aria2Package {
    target_key: &'static str,
    archive_name: &'static str,
    binary_name: &'static str,
    /// 主源：`r2d.dnof.net` 的公开分享直链（免鉴权）。
    primary_url: &'static str,
    /// 备用源：本仓库预发布 Release `tools-aria2`（与主源同一个文件）。
    fallback_url: &'static str,
    /// 压缩包 sha256（主源与备用源是同一份文件，共用一个值）。
    sha256: &'static str,
}

/// 单个aria2进程实例
#[derive(Debug)]
pub struct Aria2Instance {
    process: tokio::process::Child,
    rpc_port: u16,
    rpc_secret: String,
    active_downloads: std::sync::atomic::AtomicUsize,
    last_used: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
    health_check_failures: std::sync::atomic::AtomicUsize,
}

impl Aria2Instance {
    pub fn new(process: tokio::process::Child, rpc_port: u16, rpc_secret: String) -> Self {
        Self {
            process,
            rpc_port,
            rpc_secret,
            active_downloads: std::sync::atomic::AtomicUsize::new(0),
            last_used: std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now())),
            health_check_failures: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn get_load(&self) -> usize {
        self.active_downloads.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn increment_load(&self) {
        self.active_downloads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut last_used) = self.last_used.lock() {
            *last_used = std::time::Instant::now();
        }
    }

    pub fn decrement_load(&self) {
        self.active_downloads.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_healthy(&mut self) -> bool {
        // 检查进程是否还在运行
        match self.process.try_wait() {
            Ok(Some(_)) => {
                // 进程已退出
                debug!("aria2进程已退出 (端口: {})", self.rpc_port);
                false
            }
            Ok(None) => {
                // 进程仍在运行，重置失败计数器
                self.health_check_failures.store(0, std::sync::atomic::Ordering::SeqCst);
                true
            }
            Err(e) => {
                // 检查失败，增加失败计数
                let failure_count = self
                    .health_check_failures
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;

                if failure_count >= 3 {
                    // 连续3次失败才判定为不健康
                    warn!(
                        "aria2进程状态检查连续{}次失败 (端口: {}): {}, 判定为不健康",
                        failure_count, self.rpc_port, e
                    );
                    false
                } else {
                    // 失败次数未达到阈值，仍认为健康
                    debug!(
                        "aria2进程状态检查失败 (端口: {}, 第{}次): {}, 继续监控",
                        self.rpc_port, failure_count, e
                    );
                    true
                }
            }
        }
    }
}

pub struct Aria2Downloader {
    client: Client,
    rpc_client: ReqwestClient,
    aria2_instances: Arc<Mutex<Vec<Aria2Instance>>>,
    aria2_binary_path: PathBuf,
    instance_count: usize,
}

impl Aria2Downloader {
    fn build_rpc_client() -> Result<ReqwestClient> {
        ReqwestClient::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(20))
            .build()
            .context("failed to build aria2 RPC client")
    }

    /// 创建新的aria2下载器实例，支持多进程
    pub async fn new(client: Client) -> Result<Self> {
        tracing::info!("初始化aria2下载器...");

        // 启动前先清理所有旧的aria2进程
        tracing::debug!("清理旧的aria2进程...");
        Self::cleanup_all_aria2_processes().await;

        tracing::debug!("提取aria2可执行文件...");
        let aria2_binary_path = Self::extract_aria2_binary().await?;
        tracing::debug!("aria2可执行文件路径: {}", aria2_binary_path.display());

        // 确定进程数量：根据系统资源动态计算
        let instance_count = Self::calculate_optimal_instance_count();
        tracing::debug!("计算得出最佳aria2实例数: {}", instance_count);

        let mut downloader = Self {
            client,
            rpc_client: Self::build_rpc_client()?,
            aria2_instances: Arc::new(Mutex::new(Vec::new())),
            aria2_binary_path,
            instance_count,
        };

        // 启动所有aria2进程实例
        tracing::info!("正在启动{}aria2实例...", instance_count);
        downloader.start_all_instances().await?;
        tracing::info!("aria2实例启动完成");

        // 检查是否启用aria2健康检查
        let config = crate::config::with_config(|bundle| bundle.config.clone());

        if config.enable_aria2_health_check {
            tracing::info!("aria2健康检查已启用，正在启动监控任务...");

            // 智能健康检查监控任务
            let instances = Arc::clone(&downloader.aria2_instances);
            let instance_count = downloader.instance_count;

            // 为健康检查任务创建独立的RPC client（避免B站Client的read_timeout影响本地RPC）
            let health_check_client = downloader.rpc_client.clone();

            tokio::spawn(async move {
                // 获取用户配置
                let config = crate::config::with_config(|bundle| bundle.config.clone());
                let scan_interval = config.interval;
                let enable_auto_restart = config.enable_aria2_auto_restart;
                let check_interval = config.aria2_health_check_interval;

                // 使用用户配置的检查间隔，并进行合理范围限制
                let health_check_interval = check_interval.clamp(30, 600);

                debug!(
                    "健康检查间隔设置为 {} 秒（用户配置: {} 秒，自动重启: {}）",
                    health_check_interval, check_interval, enable_auto_restart
                );

                let mut interval = tokio::time::interval(Duration::from_secs(health_check_interval));
                let mut last_check_time = std::time::Instant::now();

                loop {
                    interval.tick().await;

                    // 检查任务暂停状态，暂停期间跳过健康检查
                    if crate::task::TASK_CONTROLLER.is_paused() {
                        debug!("任务已暂停，跳过aria2健康检查");
                        continue;
                    }

                    // 检查当前系统负载，避免在高负载时进行健康检查
                    let instances_guard = instances.lock().await;
                    let current_count = instances_guard.len();
                    let total_load: usize = instances_guard.iter().map(|i| i.get_load()).sum();
                    drop(instances_guard);

                    // 智能决策：只在系统相对空闲时进行全面健康检查
                    // 使用扫描间隔作为深度检查间隔，确保不会在扫描过程中干扰
                    if total_load == 0 && last_check_time.elapsed() > Duration::from_secs(scan_interval) {
                        // 系统完全空闲，且距离上次检查超过扫描间隔，进行全面健康检查
                        debug!(
                            "系统空闲，执行全面健康检查（距离上次检查: {}秒）",
                            last_check_time.elapsed().as_secs()
                        );

                        // 执行完整的智能健康检查
                        if let Err(e) = Self::smart_health_check(&health_check_client, &instances, instance_count).await
                        {
                            warn!("全面健康检查失败: {:#}", e);
                        } else {
                            debug!("全面健康检查完成");
                        }

                        last_check_time = std::time::Instant::now();
                    } else if current_count < instance_count {
                        // 实例数量不足，检查是否启用自动恢复
                        if enable_auto_restart && total_load < 2 {
                            // 启用自动重启且负载较低时立即尝试恢复
                            warn!(
                                "检测到aria2实例数量不足: {}/{} (当前负载: {})，立即尝试自动恢复",
                                current_count, instance_count, total_load
                            );

                            // 计算需要创建的实例数量
                            let missing_count = instance_count - current_count;

                            // 尝试创建缺失的实例
                            for i in 0..missing_count {
                                match Self::create_missing_instance(&instances).await {
                                    Ok(new_instance) => {
                                        let mut instances_guard = instances.lock().await;
                                        instances_guard.push(new_instance);
                                        debug!("成功恢复第{}个aria2实例", i + 1);
                                    }
                                    Err(e) => {
                                        error!("恢复第{}个aria2实例失败: {:#}", i + 1, e);
                                        // 记录详细的失败原因以便诊断
                                        Self::log_startup_diagnostics().await;
                                    }
                                }
                            }
                        } else if !enable_auto_restart {
                            debug!(
                                "aria2实例数量不足: {}/{} (负载: {})，但自动重启已禁用",
                                current_count, instance_count, total_load
                            );
                        } else {
                            debug!(
                                "aria2实例数量不足但系统忙碌: {}/{} (负载: {})，暂缓处理",
                                current_count, instance_count, total_load
                            );
                        }
                    }
                }
            });

            debug!("aria2实例监控任务已启动");
        } else {
            debug!("aria2健康检查已禁用，跳过监控任务启动");
        }

        Ok(downloader)
    }

    /// 计算最优的aria2进程数量
    fn calculate_optimal_instance_count() -> usize {
        // Force single aria2 instance; use internal split/connection for parallelism.
        1
    }

    /// 清理所有aria2进程 (Windows兼容)
    async fn cleanup_all_aria2_processes() {
        debug!("清理所有旧的aria2进程...");

        #[cfg(target_os = "windows")]
        {
            // Windows: 使用taskkill强制终止所有aria2进程
            let output = crate::utils::process::tokio_command("taskkill")
                .args(["/F", "/IM", "aria2c.exe"])
                .output()
                .await;

            match output {
                Ok(result) => {
                    if result.status.success() {
                        // Windows taskkill 输出使用系统默认编码，不直接解码以避免乱码
                        debug!("Windows aria2进程清理完成");
                    } else {
                        debug!("Windows aria2进程清理出现问题，但进程可能已终止");
                    }
                }
                Err(e) => {
                    warn!("Windows aria2进程清理失败: {}", e);
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            // Linux: 使用pkill强制终止
            let output = crate::utils::process::tokio_command("pkill")
                .args(["-9", "-f", "aria2c"])
                .output()
                .await;

            match output {
                Ok(result) => {
                    if result.status.success() {
                        debug!("Linux aria2进程清理完成");
                    } else {
                        debug!("Linux aria2进程清理: 没有找到运行中的aria2进程");
                    }
                }
                Err(e) => {
                    debug!("Linux aria2进程清理失败: {}", e);
                }
            }
        }

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // macOS: 使用pkill强制终止
            let output = crate::utils::process::tokio_command("pkill")
                .args(["-9", "-f", "aria2c"])
                .output()
                .await;

            match output {
                Ok(result) => {
                    if result.status.success() {
                        debug!("macOS aria2进程清理完成");
                    } else {
                        debug!("macOS aria2进程清理: 没有找到运行中的aria2进程");
                    }
                }
                Err(e) => {
                    debug!("macOS aria2进程清理失败: {}", e);
                }
            }
        }

        // 等待进程完全终止
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }

    /// 计算单个实例的最优线程数
    fn calculate_threads_per_instance(total_threads: usize, instance_count: usize) -> usize {
        let base_threads = total_threads / instance_count;
        let remainder = total_threads % instance_count;

        // 基础分配 + 考虑余数
        let threads_per_instance = if remainder > 0 { base_threads + 1 } else { base_threads };

        // 智能限制：根据线程数量动态调整上限
        let max_threads_per_instance = match total_threads {
            1..=16 => total_threads, // 小量线程不限制
            17..=32 => 16,           // 中等线程限制到16
            33..=64 => 20,           // 较多线程限制到20
            65..=128 => 24,          // 大量线程限制到24
            _ => 32,                 // 超大量线程限制到32
        };

        std::cmp::min(threads_per_instance, max_threads_per_instance)
    }

    /// 根据文件大小智能调整线程数
    fn calculate_smart_threads_for_file(file_size_mb: u64, base_threads: usize, total_threads: usize) -> usize {
        let smart_threads = match file_size_mb {
            0..=2 => 1,                                    // 极小文件单线程足够
            3..=10 => std::cmp::min(base_threads, 2),      // 小文件用少量线程
            11..=50 => std::cmp::min(base_threads, 4),     // 中等文件适中线程
            51..=200 => std::cmp::min(base_threads, 8),    // 大文件较多线程
            201..=1000 => std::cmp::min(base_threads, 12), // 很大文件更多线程
            _ => {
                // 超大文件(>1GB): 可以使用更多线程，突破单实例限制
                let max_for_large_file = std::cmp::min(total_threads * 3 / 4, 16);
                std::cmp::max(base_threads, std::cmp::min(max_for_large_file, total_threads))
            }
        };

        std::cmp::max(smart_threads, 1) // 至少1个线程
    }

    /// 尝试获取文件大小（用于智能线程调整），带超时控制
    async fn try_get_file_size(&self, url: &str, referer: Option<&str>, proxy: Option<&str>) -> Option<u64> {
        let result = timeout(Duration::from_secs(5), async {
            let mut headers = create_api_headers();
            if let Ok(range) = "bytes=0-0".parse() {
                headers.insert("Range", range);
            }
            if let Some(referer) = referer {
                if let Ok(referer) = referer.parse() {
                    headers.insert("Referer", referer);
                }
            }

            let client = match proxy.filter(|value| !value.trim().is_empty()) {
                Some(proxy) => self.client.with_media_proxy(proxy)?,
                None => self.client.clone(),
            };
            let response = client.media_request(Method::GET, url).headers(headers).send().await?;
            Ok::<_, anyhow::Error>(response)
        })
        .await;

        match result {
            Ok(Ok(response)) => {
                let headers = response.headers();
                if let Some(total) = headers
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(Self::parse_content_range_total)
                {
                    return Some(total);
                }

                headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
            }
            Ok(Err(e)) => {
                debug!("获取文件大小失败: {:#}", e);
                None
            }
            Err(_) => {
                debug!("获取文件大小超时");
                None
            }
        }
    }

    fn is_media_like_file(file_name: &str) -> bool {
        let name = file_name.to_ascii_lowercase();
        name.ends_with(".tmp_video")
            || name.ends_with(".tmp_audio")
            || name.ends_with(".m4s")
            || name.ends_with(".mp4")
            || name.ends_with(".flv")
            || name.ends_with(".mkv")
            || name.ends_with(".ts")
            || name.ends_with(".mp3")
            || name.ends_with(".aac")
            || name.ends_with(".flac")
            || name.ends_with(".wav")
            || name.ends_with(".ogg")
    }

    fn parse_content_range_total(value: &str) -> Option<u64> {
        let total_part = value.split('/').nth(1)?;
        total_part.parse::<u64>().ok()
    }
    /// 启动所有aria2进程实例
    async fn start_all_instances(&mut self) -> Result<()> {
        let mut instances = Vec::new();

        for i in 0..self.instance_count {
            let rpc_port = Self::find_available_port().await?;
            let rpc_secret = Self::generate_secret();

            info!("启动第 {} 个aria2进程，端口: {}", i + 1, rpc_port);

            let process = self.start_single_instance(rpc_port, &rpc_secret).await?;
            let instance = Aria2Instance::new(process, rpc_port, rpc_secret);

            // 等待aria2 RPC服务完全启动（关键修复：避免过早检查）
            info!("等待aria2实例 {} RPC服务启动...", i + 1);
            tokio::time::sleep(Duration::from_secs(3)).await;

            // 验证连接（带重试）
            if let Err(e) = self.test_instance_connection(rpc_port, &instance.rpc_secret).await {
                warn!("aria2实例 {} 连接测试失败: {:#}", i + 1, e);
                continue;
            }

            instances.push(instance);
            info!("aria2实例 {} 启动成功", i + 1);
        }

        if instances.is_empty() {
            bail!("没有成功启动任何aria2实例");
        }

        *self.aria2_instances.lock().await = instances;
        info!("成功启动 {} 个aria2实例", self.aria2_instances.lock().await.len());

        Ok(())
    }

    fn install_lock() -> &'static Mutex<()> {
        ARIA2_INSTALL_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn current_package() -> Option<Aria2Package> {
        Some(Aria2Package {
            target_key: "windows-x86_64",
            archive_name: "aria2-1.37.0-win-64bit-build1.zip",
            binary_name: "aria2c.exe",
            primary_url: "https://r2d.dnof.net/files/tools/aria2/aria2-1.37.0-win-64bit-build1.zip",
            fallback_url:
                "https://github.com/NeeYoonc/bili-sync-up/releases/download/tools-aria2/aria2-1.37.0-win-64bit-build1.zip",
            sha256: "67d015301eef0b612191212d564c5bb0a14b5b9c4796b76454276a4d28d9b288",
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn current_package() -> Option<Aria2Package> {
        Some(Aria2Package {
            target_key: "linux-x86_64",
            archive_name: "aria2-x86_64-linux-musl_static.zip",
            binary_name: "aria2c",
            primary_url: "https://r2d.dnof.net/files/tools/aria2/aria2-x86_64-linux-musl_static.zip",
            fallback_url:
                "https://github.com/NeeYoonc/bili-sync-up/releases/download/tools-aria2/aria2-x86_64-linux-musl_static.zip",
            sha256: "0d86b4fe16bc820f9e97ccd1034c88db87b884ed59b8db3ba7ebf286424389f4",
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    fn current_package() -> Option<Aria2Package> {
        Some(Aria2Package {
            target_key: "linux-aarch64",
            archive_name: "aria2-aarch64-linux-musl_static.zip",
            binary_name: "aria2c",
            primary_url: "https://r2d.dnof.net/files/tools/aria2/aria2-aarch64-linux-musl_static.zip",
            fallback_url:
                "https://github.com/NeeYoonc/bili-sync-up/releases/download/tools-aria2/aria2-aarch64-linux-musl_static.zip",
            sha256: "0bdd04973526b7c8afddf8b63b89b456eb1a56a9c4a73e109f70773f712f6fe7",
        })
    }

    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    fn current_package() -> Option<Aria2Package> {
        None
    }

    fn cache_dir(package: Aria2Package) -> PathBuf {
        CONFIG_DIR
            .join("tools")
            .join("aria2")
            .join(ARIA2_VERSION)
            .join(package.target_key)
    }

    /// 解析缓存或按需下载 aria2，失败时回退到系统 aria2
    async fn extract_aria2_binary() -> Result<PathBuf> {
        let Some(package) = Self::current_package() else {
            debug!("当前平台没有预置 aria2 下载包，尝试使用系统 aria2");
            return Self::find_system_aria2().await;
        };

        let cache_dir = Self::cache_dir(package);
        let binary_path = cache_dir.join(package.binary_name);

        if Self::is_valid_aria2_binary(&binary_path).await {
            return Ok(binary_path);
        }

        if let Ok(system_path) = Self::find_system_aria2().await {
            return Ok(system_path);
        }

        let _guard = Self::install_lock().lock().await;

        if Self::is_valid_aria2_binary(&binary_path).await {
            return Ok(binary_path);
        }

        if let Ok(system_path) = Self::find_system_aria2().await {
            return Ok(system_path);
        }

        Self::download_and_install_aria2(package).await
    }

    async fn download_and_install_aria2(package: Aria2Package) -> Result<PathBuf> {
        let cache_dir = Self::cache_dir(package);
        let archive_path = cache_dir.join(package.archive_name);
        let binary_path = cache_dir.join(package.binary_name);
        // 候选下载源：镜像（设置环境变量时单源）或 主源 r2d → 备用源 GitHub Release。
        let sources = plan_aria2_download_sources(&package, aria2_mirror_base().as_deref());
        let manual_hint = format!(
            "可手动下载 {} 并解压出 {} 放到 {}，或设置 {} 指向含同名压缩包的镜像目录",
            package.archive_name,
            package.binary_name,
            binary_path.display(),
            ARIA2_MIRROR_BASE_ENV
        );

        tokio::fs::create_dir_all(&cache_dir)
            .await
            .with_context(|| format!("创建 aria2 缓存目录失败: {}", cache_dir.display()))?;

        info!(
            "开始按需下载 aria2 二进制: version={}, target={}, 候选源={:?}",
            ARIA2_VERSION,
            package.target_key,
            sources.iter().map(|source| source.label).collect::<Vec<_>>()
        );

        let download_client = ReqwestClient::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(ARIA2_DOWNLOAD_TIMEOUT)
            .build()
            .context("创建 aria2 下载客户端失败")?;
        let mut downloaded = None;
        let mut failures: Vec<(String, String)> = Vec::new();

        for (index, source) in sources.iter().enumerate() {
            match Self::download_aria2_archive(&download_client, source).await {
                Ok(bytes) => {
                    info!("aria2 压缩包已从「{}」下载完成: {}", source.label, source.url);
                    downloaded = Some(bytes);
                    break;
                }
                Err(error) => {
                    let has_next = next_aria2_source(&sources, index).is_some();
                    warn!(
                        "aria2 下载源「{}」失败：{:#}；{}",
                        source.label,
                        error,
                        if has_next {
                            "继续尝试下一个源"
                        } else {
                            "没有更多可用源"
                        }
                    );
                    failures.push((source.label.to_string(), format!("{error:#}")));
                }
            }
        }

        let Some(archive_bytes) = downloaded else {
            bail!("{}", aria2_download_failure_message(&failures, &manual_hint));
        };

        tokio::fs::write(&archive_path, archive_bytes.as_slice())
            .await
            .with_context(|| format!("写入 aria2 压缩包失败: {}", archive_path.display()))?;

        Self::extract_binary_from_zip(&archive_path, &binary_path, package.binary_name).await?;
        Self::set_executable_permissions(&binary_path).await?;

        if Self::is_valid_aria2_binary(&binary_path).await {
            info!("aria2 已缓存到本地: {}", binary_path.display());
            return Ok(binary_path);
        }

        let _ = tokio::fs::remove_file(&binary_path).await;
        bail!("aria2 安装完成后校验失败: {}", binary_path.display())
    }

    /// 从单个下载源拉取并校验 aria2 压缩包。
    async fn download_aria2_archive(client: &ReqwestClient, source: &Aria2DownloadSource) -> Result<Vec<u8>> {
        let response = client
            .get(&source.url)
            .send()
            .await
            .with_context(|| format!("请求下载源失败: {}", source.url))?
            .error_for_status()
            .with_context(|| format!("下载源返回错误状态: {}", source.url))?;
        let archive_bytes = response.bytes().await.context("读取 aria2 下载内容失败")?;
        Self::verify_archive_checksum(archive_bytes.as_ref(), source.sha256)
            .with_context(|| format!("aria2 压缩包校验未通过: {}", source.url))?;
        Ok(archive_bytes.to_vec())
    }

    fn verify_archive_checksum(bytes: &[u8], expected_sha256: &str) -> Result<()> {
        let actual_sha256 = hex::encode(Sha256::digest(bytes));
        if actual_sha256 != expected_sha256 {
            bail!(
                "aria2 压缩包校验失败: expected={}, actual={}",
                expected_sha256,
                actual_sha256
            );
        }
        Ok(())
    }

    async fn extract_binary_from_zip(archive_path: &Path, binary_path: &Path, binary_name: &str) -> Result<()> {
        let archive_path = archive_path.to_path_buf();
        let binary_path = binary_path.to_path_buf();
        let binary_name = binary_name.to_string();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let archive_file = std::fs::File::open(&archive_path)
                .with_context(|| format!("打开 aria2 压缩包失败: {}", archive_path.display()))?;
            let mut archive = ZipArchive::new(archive_file).context("解析 aria2 zip 压缩包失败")?;

            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).context("读取 aria2 zip 条目失败")?;
                let entry_name = entry.name().to_string();
                let Some(file_name) = Path::new(&entry_name).file_name().and_then(|name| name.to_str()) else {
                    continue;
                };

                if file_name != binary_name {
                    continue;
                }

                if let Some(parent) = binary_path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("创建 aria2 解压目录失败: {}", parent.display()))?;
                }

                let mut output = std::fs::File::create(&binary_path)
                    .with_context(|| format!("创建 aria2 可执行文件失败: {}", binary_path.display()))?;
                std::io::copy(&mut entry, &mut output)
                    .with_context(|| format!("写入 aria2 可执行文件失败: {}", binary_path.display()))?;
                return Ok(());
            }

            bail!(
                "aria2 压缩包中未找到目标文件: {} ({})",
                binary_name,
                archive_path.display()
            )
        })
        .await
        .context("解压 aria2 压缩包失败")??;

        Ok(())
    }

    async fn set_executable_permissions(binary_path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let metadata = tokio::fs::metadata(binary_path)
                .await
                .with_context(|| format!("读取 aria2 文件元数据失败: {}", binary_path.display()))?;
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            tokio::fs::set_permissions(binary_path, permissions)
                .await
                .with_context(|| format!("设置 aria2 执行权限失败: {}", binary_path.display()))?;
        }

        #[cfg(not(unix))]
        {
            let _ = binary_path;
        }

        Ok(())
    }

    /// 验证aria2二进制文件是否有效
    async fn is_valid_aria2_binary(path: &Path) -> bool {
        if !path.exists() {
            warn!("aria2二进制文件不存在: {}", path.display());
            return false;
        }

        // 尝试执行 aria2c --version 来验证
        match crate::utils::process::tokio_command(path)
            .arg("--version")
            .output()
            .await
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if output.status.success() && stdout.contains("aria2") {
                    debug!("aria2二进制文件验证成功: {}", path.display());
                    true
                } else {
                    warn!(
                        "aria2二进制文件验证失败: {}，退出码: {:?}，stdout: {}，stderr: {}",
                        path.display(),
                        output.status.code(),
                        stdout.trim(),
                        stderr.trim()
                    );
                    false
                }
            }
            Err(e) => {
                warn!("无法执行aria2二进制文件 {}: {}", path.display(), e);
                false
            }
        }
    }

    /// 查找系统安装的aria2
    async fn find_system_aria2() -> Result<PathBuf> {
        let _binary_name = if cfg!(target_os = "windows") {
            "aria2c.exe"
        } else {
            "aria2c"
        };

        // 尝试使用which命令查找
        match crate::utils::process::tokio_command("which")
            .arg("aria2c")
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let system_path = PathBuf::from(path_str);

                if Self::is_valid_aria2_binary(&system_path).await {
                    info!("使用系统安装的aria2: {}", system_path.display());
                    return Ok(system_path);
                }
            }
            _ => {}
        }

        // 在Windows上尝试where命令
        #[cfg(target_os = "windows")]
        {
            match crate::utils::process::tokio_command("where")
                .arg("aria2c")
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    let system_path = PathBuf::from(path_str);

                    if Self::is_valid_aria2_binary(&system_path).await {
                        info!("使用系统安装的aria2: {}", system_path.display());
                        return Ok(system_path);
                    }
                }
                _ => {}
            }
        }

        // 尝试常见的安装路径
        let common_paths = if cfg!(target_os = "windows") {
            vec![
                PathBuf::from("C:\\Program Files\\aria2\\aria2c.exe"),
                PathBuf::from("C:\\Program Files (x86)\\aria2\\aria2c.exe"),
            ]
        } else {
            vec![
                PathBuf::from("/usr/bin/aria2c"),
                PathBuf::from("/usr/local/bin/aria2c"),
                PathBuf::from("/opt/homebrew/bin/aria2c"),
            ]
        };

        for path in common_paths {
            if Self::is_valid_aria2_binary(&path).await {
                info!("使用系统安装的aria2: {}", path.display());
                return Ok(path);
            }
        }

        bail!("未找到可用的aria2二进制文件，请确保系统已安装aria2")
    }

    /// 查找可用的端口
    async fn find_available_port() -> Result<u16> {
        use std::net::TcpListener;

        // 尝试绑定到随机端口
        let listener = TcpListener::bind("127.0.0.1:0").context("Failed to bind to random port")?;
        let port = listener.local_addr()?.port();
        drop(listener);

        Ok(port)
    }

    /// 生成随机密钥
    fn generate_secret() -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut hasher = DefaultHasher::new();
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .hash(&mut hasher);

        format!("bili-sync-{:x}", hasher.finish())
    }

    /// 启动单个aria2实例
    async fn start_single_instance(&self, rpc_port: u16, rpc_secret: &str) -> Result<tokio::process::Child> {
        let current_config = crate::config::reload_config();
        let total_threads = current_config.concurrent_limit.parallel_download.threads;

        // 智能计算当前实例应该使用的线程数
        let threads = Self::calculate_threads_per_instance(total_threads, self.instance_count);

        info!(
            "启动aria2实例，分配线程数: {} (总线程: {}, 实例数: {})",
            threads, total_threads, self.instance_count
        );

        let mut args = vec![
            "--enable-rpc".to_string(),
            format!("--rpc-listen-port={}", rpc_port),
            "--rpc-allow-origin-all".to_string(),
            format!("--rpc-secret={}", rpc_secret),
            "--continue=true".to_string(),
            format!("--max-connection-per-server={}", threads),
            "--min-split-size=1M".to_string(),
            format!("--split={}", threads),
            "--max-concurrent-downloads=6".to_string(), // 每个实例最多6个文件
            "--disable-ipv6=true".to_string(),
            // 增强的网络容错配置
            "--max-tries=8".to_string(),        // 增加重试次数
            "--retry-wait=5".to_string(),       // 增加重试间隔
            "--timeout=60".to_string(),         // 增加整体超时
            "--connect-timeout=20".to_string(), // 增加连接超时
            "--auto-file-renaming=false".to_string(),
            "--allow-overwrite=true".to_string(),
            // DNS优化配置
            "--async-dns=true".to_string(),
            "--async-dns-server=8.8.8.8,1.1.1.1,223.5.5.5,114.114.114.114".to_string(),
            "--enable-async-dns6=false".to_string(),
            // 网络优化配置
            "--lowest-speed-limit=1K".to_string(),
            "--max-overall-download-limit=0".to_string(),
            "--stream-piece-selector=geom".to_string(),
            "--piece-length=1M".to_string(),
            "--summary-interval=0".to_string(),
            "--quiet=true".to_string(),
        ];

        // 添加SSL/TLS相关配置
        if cfg!(target_os = "linux") {
            let ca_paths = [
                "/etc/ssl/certs/ca-certificates.crt",
                "/etc/pki/tls/certs/ca-bundle.crt",
                "/etc/ssl/ca-bundle.pem",
                "/etc/ssl/cert.pem",
            ];

            let mut ca_found = false;
            for ca_path in &ca_paths {
                if std::path::Path::new(ca_path).exists() {
                    args.push(format!("--ca-certificate={}", ca_path));
                    ca_found = true;
                    break;
                }
            }

            if !ca_found {
                args.push("--check-certificate=false".to_string());
            }
        } else {
            args.push("--check-certificate=false".to_string());
        }

        // 启动aria2进程，保留stderr用于诊断
        let mut child = crate::utils::process::tokio_command(&self.aria2_binary_path)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped()) // 保留stderr用于错误诊断
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to start aria2 daemon: binary={}, port={}, args={:?}",
                    self.aria2_binary_path.display(),
                    rpc_port,
                    args
                )
            })?;

        // 检查进程是否立即退出
        tokio::time::sleep(Duration::from_millis(500)).await;
        match child.try_wait() {
            Ok(Some(exit_status)) => {
                // 进程立即退出，尝试读取错误信息
                let mut stderr_output = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut stderr_output).await;
                }

                error!(
                    "aria2进程立即退出，退出码: {:?}, stderr: {}",
                    exit_status.code(),
                    stderr_output
                );
                bail!(
                    "aria2进程启动失败，立即退出，退出码: {:?}, 错误信息: {}",
                    exit_status.code(),
                    stderr_output
                );
            }
            Ok(None) => {
                // 进程正在运行，移除stderr避免影响正常运行
                child.stderr.take();
                debug!("aria2进程启动成功，端口: {}", rpc_port);
            }
            Err(e) => {
                warn!("无法检查aria2进程状态: {}", e);
            }
        }

        Ok(child)
    }

    /// 测试单个实例的连接（带重试机制）
    async fn test_instance_connection(&self, rpc_port: u16, rpc_secret: &str) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/jsonrpc", rpc_port);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "aria2.getVersion",
            "id": "test",
            "params": [format!("token:{}", rpc_secret)]
        });

        // 使用重试机制测试连接 - 针对启动时机优化
        self.retry_with_backoff(
            "连接测试",
            5,                      // 减少重试次数，因为已经预等待了
            Duration::from_secs(1), // 增加重试间隔，给RPC更多时间
            Duration::from_secs(8), // 适度增加单次超时
            || async {
                let response = self.rpc_client.post(&url).json(&payload).send().await?;
                if response.status().is_success() {
                    Ok(())
                } else {
                    bail!("连接测试返回错误状态: {}", response.status())
                }
            },
        )
        .await
    }

    /// 选择最佳aria2实例（负载均衡+健康检查）
    async fn select_best_instance(&self) -> Result<(usize, u16, String)> {
        let instances = self.aria2_instances.lock().await;

        if instances.is_empty() {
            if crate::task::TASK_CONTROLLER.is_paused() {
                bail!("用户主动暂停任务");
            }
            bail!("没有可用的aria2实例");
        }

        // 优化：只检查进程状态，避免频繁RPC检查
        let mut healthy_instances = Vec::new();
        for (index, instance) in instances.iter().enumerate() {
            // 只进行基础进程健康检查，避免频繁RPC调用
            // RPC健康检查移到定期的health_check中进行
            healthy_instances.push((index, instance));
        }

        if healthy_instances.is_empty() {
            warn!("所有aria2实例都不健康，尝试使用第一个实例");
            let instance = &instances[0];
            return Ok((0, instance.rpc_port, instance.rpc_secret.clone()));
        }

        // 找到负载最低的健康实例
        let (best_index, best_instance) = healthy_instances
            .iter()
            .min_by_key(|(_, instance)| instance.get_load())
            .ok_or_else(|| anyhow::anyhow!("无法找到可用实例"))?;

        Ok((*best_index, best_instance.rpc_port, best_instance.rpc_secret.clone()))
    }

    /// 使用aria2下载文件，支持多个URL备选和多进程
    pub async fn fetch_with_aria2_fallback(&self, urls: &[&str], path: &Path) -> Result<()> {
        self.fetch_with_aria2_fallback_and_optional_headers(urls, path, None, None, None)
            .await
    }

    /// 仅为当前 aria2 任务设置代理，不修改 aria2 进程级或全局代理。
    pub async fn fetch_with_aria2_fallback_with_proxy(&self, urls: &[&str], path: &Path, proxy: &str) -> Result<()> {
        self.fetch_with_aria2_fallback_and_optional_headers(
            urls,
            path,
            Some("https://www.youtube.com/"),
            None,
            Some(proxy),
        )
        .await
    }

    pub async fn fetch_with_aria2_fallback_with_referer(
        &self,
        urls: &[&str],
        path: &Path,
        referer: &str,
    ) -> Result<()> {
        self.fetch_with_aria2_fallback_and_optional_headers(urls, path, Some(referer), None, None)
            .await
    }

    pub async fn fetch_with_aria2_fallback_with_referer_and_cookie(
        &self,
        urls: &[&str],
        path: &Path,
        referer: &str,
        cookie: &str,
    ) -> Result<()> {
        if cookie.trim().is_empty() {
            return self.fetch_with_aria2_fallback_with_referer(urls, path, referer).await;
        }
        self.fetch_with_aria2_fallback_and_optional_headers(urls, path, Some(referer), Some(cookie), None)
            .await
    }

    async fn fetch_with_aria2_fallback_and_optional_headers(
        &self,
        urls: &[&str],
        path: &Path,
        referer: Option<&str>,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<()> {
        if urls.is_empty() {
            bail!("No URLs provided");
        }
        if crate::task::TASK_CONTROLLER.is_paused() {
            bail!("用户主动暂停任务");
        }

        // 尝试删除已存在的文件以确保重新下载（忽略文件不存在的错误）
        // 使用 remove_file 而不是 exists() 检查，避免Windows路径混合斜杠导致的问题
        match tokio::fs::remove_file(path).await {
            Ok(_) => debug!("删除已存在的文件以重新下载: {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 文件不存在，继续下载
            }
            Err(e) => {
                warn!("删除文件失败: {}, 错误: {:#}", path.display(), e);
                // 即使删除失败也继续尝试下载，aria2会覆盖
            }
        }

        // 确保目标目录存在
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let file_name = path.file_name().and_then(|n| n.to_str()).context("Invalid file name")?;
        let dir = path
            .parent()
            .and_then(|p| p.to_str())
            .context("Invalid directory path")?;

        // 选择最佳的aria2实例
        let (instance_index, rpc_port, rpc_secret) = self.select_best_instance().await?;

        info!(
            "使用aria2实例 {} (端口: {}) 下载: {}",
            instance_index + 1,
            rpc_port,
            file_name
        );

        // 增加该实例的负载计数
        {
            let instances = self.aria2_instances.lock().await;
            if let Some(instance) = instances.get(instance_index) {
                instance.increment_load();
            }
        }

        // 构建aria2 RPC请求
        let gid = self
            .add_download_task_to_instance(
                urls,
                dir,
                file_name,
                &Aria2Rpc {
                    port: rpc_port,
                    secret: &rpc_secret,
                },
                &RequestHints { referer, cookie, proxy },
            )
            .await?;

        // 等待下载完成
        let result = self
            .wait_for_download_on_instance(&gid, rpc_port, &rpc_secret, instance_index)
            .await;

        // 减少该实例的负载计数
        {
            let instances = self.aria2_instances.lock().await;
            if let Some(instance) = instances.get(instance_index) {
                instance.decrement_load();
            }
        }

        // 检查下载结果
        result?;

        // 增强的文件验证逻辑
        self.verify_downloaded_file(path).await?;

        Ok(())
    }

    /// 添加下载任务到指定实例（带重试机制）
    async fn add_download_task_to_instance(
        &self,
        urls: &[&str],
        dir: &str,
        file_name: &str,
        rpc: &Aria2Rpc<'_>,
        hints: &RequestHints<'_>,
    ) -> Result<String> {
        let rpc_port = rpc.port;
        let rpc_secret = rpc.secret;
        let referer = hints.referer;
        let cookie = hints.cookie;
        let proxy = hints.proxy;
        let url = format!("http://127.0.0.1:{}/jsonrpc", rpc_port);

        // 智能计算当前实例的线程数
        let current_config = crate::config::reload_config();
        let total_threads = current_config.concurrent_limit.parallel_download.threads;
        let base_threads_default = Self::calculate_threads_per_instance(total_threads, self.instance_count);
        let active_instances = {
            let instances = self.aria2_instances.lock().await;
            instances.iter().filter(|i| i.get_load() > 0).count()
        };
        let base_threads = if active_instances <= 1 {
            total_threads
        } else {
            base_threads_default
        };

        // 尝试获取文件大小，并根据大小智能调整线程数
        let threads = if let Some(file_size_bytes) = self.try_get_file_size(urls[0], referer, proxy).await {
            let file_size_mb = file_size_bytes / 1_048_576; // MB
            if file_size_mb <= 2 && Self::is_media_like_file(file_name) {
                base_threads
            } else {
                let smart_threads = Self::calculate_smart_threads_for_file(file_size_mb, base_threads, total_threads);
                debug!(
                    "file_size_mb={}MB, smart_threads={}, base_threads={}, total_threads={}",
                    file_size_mb, smart_threads, base_threads, total_threads
                );
                smart_threads
            }
        } else {
            debug!("无法获取文件大小，使用基础线程数: {}", base_threads);
            base_threads
        };

        // 构建基础选项 - 增强网络容错
        tracing::debug!(
            "配置aria2下载任务 - 文件: {}, 线程数: {}, 目标目录: {}",
            file_name,
            threads,
            dir
        );
        tracing::debug!("aria2下载链接数量: {}", urls.len());
        for (i, url) in urls.iter().enumerate() {
            tracing::debug!("aria2下载链接{}: {}", i + 1, url);
        }

        let mut aria2_headers = referer
            .map(create_aria2_headers_with_referer)
            .unwrap_or_else(create_aria2_headers);
        if let Some(cookie) = cookie {
            aria2_headers.push(format!("Cookie: {cookie}"));
        }
        let mut options = serde_json::json!({
            "dir": dir,
            "out": file_name,
            "continue": "true",
            "max-connection-per-server": threads.to_string(),
            "split": threads.to_string(),
            "min-split-size": "1M",
            // 增强的网络容错配置
            "max-tries": "8",
            "retry-wait": "5",
            "timeout": "60",
            "connect-timeout": "20",
            "auto-file-renaming": "false",
            "allow-overwrite": "true",
            // DNS优化配置
            "async-dns": "true",
            "async-dns-server": ["8.8.8.8", "1.1.1.1", "223.5.5.5", "114.114.114.114"],
            "enable-async-dns6": "false",
            // 网络优化配置
            "lowest-speed-limit": "1K",
            "stream-piece-selector": "geom",
            "piece-length": "1M",
            "header": aria2_headers
        });
        if let Some(proxy) = proxy.map(str::trim).filter(|value| !value.is_empty()) {
            options["all-proxy"] = serde_json::Value::String(proxy.to_string());
        }

        // 添加SSL/TLS相关配置
        if cfg!(target_os = "linux") {
            let ca_paths = [
                "/etc/ssl/certs/ca-certificates.crt", // Debian/Ubuntu
                "/etc/pki/tls/certs/ca-bundle.crt",   // RHEL/CentOS
                "/etc/ssl/ca-bundle.pem",             // openSUSE
                "/etc/ssl/cert.pem",                  // Alpine
            ];

            let mut ca_found = false;
            for ca_path in &ca_paths {
                if std::path::Path::new(ca_path).exists() {
                    options["ca-certificate"] = serde_json::Value::String(ca_path.to_string());
                    ca_found = true;
                    break;
                }
            }

            if !ca_found {
                options["check-certificate"] = serde_json::Value::String("false".to_string());
            }
        } else {
            options["check-certificate"] = serde_json::Value::String("false".to_string());
        }

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "aria2.addUri",
            "id": "add_download",
            "params": [
                format!("token:{}", rpc_secret),
                urls,
                options
            ]
        });

        // 使用重试机制添加下载任务
        let gid = self
            .retry_with_backoff(
                "添加下载任务",
                3,
                Duration::from_millis(1000),
                Duration::from_secs(20),
                || async {
                    tracing::debug!("发送aria2 API请求 - URL: {}", url);
                    tracing::debug!(
                        "aria2 API请求载荷: {}",
                        serde_json::to_string_pretty(&payload).unwrap_or_default()
                    );

                    let response = self
                        .rpc_client
                        .post(&url)
                        .json(&payload)
                        .send()
                        .await
                        .context("发送添加下载任务请求失败")?;

                    tracing::debug!("aria2 API响应状态: {}", response.status());

                    let json: serde_json::Value = response.json().await?;
                    tracing::debug!(
                        "aria2 API响应数据: {}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );

                    if let Some(error) = json.get("error") {
                        bail!("aria2 API错误: {}", error);
                    }

                    let gid = json["result"].as_str().context("aria2返回无效的GID")?;

                    Ok(gid.to_string())
                },
            )
            .await?;

        info!("开始aria2下载: {} (线程数: {}, GID: {})", file_name, threads, gid);
        Ok(gid)
    }

    /// 在指定实例上等待下载完成（优化版状态检查）
    async fn wait_for_download_on_instance(
        &self,
        gid: &str,
        rpc_port: u16,
        rpc_secret: &str,
        _instance_index: usize,
    ) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/jsonrpc", rpc_port);
        let mut consecutive_failures = 0;
        const MAX_CONSECUTIVE_FAILURES: u32 = 5;

        // 优化：智能状态检查参数
        #[allow(unused_assignments)]
        let mut check_interval = Duration::from_secs(1); // 初始1秒检查，减少频率
        let mut last_completed_length = 0u64;
        let mut stall_count = 0;
        let start_time = std::time::Instant::now();

        // 下载超时保护：默认30分钟，大文件动态调整
        let download_timeout = Duration::from_secs(30 * 60); // 30分钟

        loop {
            // 检查任务暂停状态，如果暂停则立即退出下载等待
            if crate::task::TASK_CONTROLLER.is_paused() {
                info!("用户暂停任务，停止下载状态检查 (GID: {})", gid);
                bail!("用户主动暂停任务");
            }

            // 检查总体超时
            if start_time.elapsed() > download_timeout {
                warn!(
                    "下载超时 (GID: {})，已等待 {:.1} 分钟",
                    gid,
                    start_time.elapsed().as_secs_f64() / 60.0
                );
                bail!("下载超时，超过{}分钟", download_timeout.as_secs() / 60);
            }

            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "aria2.tellStatus",
                "id": "check_status",
                "params": [
                    format!("token:{}", rpc_secret),
                    gid,
                    ["status", "totalLength", "completedLength", "downloadSpeed", "errorMessage"]
                ]
            });

            // 单次状态检查带超时和重试
            let check_result = self
                .retry_with_backoff(
                    "状态检查",
                    2, // 减少重试次数，提高响应速度
                    Duration::from_millis(500),
                    Duration::from_secs(20), // 进一步增加超时时间，降低在慢系统/卡盘场景下的误报
                    || async {
                        let response = self
                            .rpc_client
                            .post(&url)
                            .json(&payload)
                            .send()
                            .await
                            .context("发送状态检查请求失败")?;

                        let json: serde_json::Value = response.json().await?;

                        if let Some(error) = json.get("error") {
                            bail!("aria2状态检查错误: {}", error);
                        }

                        Ok(json)
                    },
                )
                .await;

            let json = match check_result {
                Ok(json) => {
                    consecutive_failures = 0;
                    json
                }
                Err(e) => {
                    consecutive_failures += 1;
                    warn!(
                        "状态检查失败 ({}/{}): {:#}",
                        consecutive_failures, MAX_CONSECUTIVE_FAILURES, e
                    );

                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        return Err(e.context("连续状态检查失败次数过多，放弃下载"));
                    }

                    // 失败时使用更长的等待时间
                    sleep(Duration::from_millis(3000)).await;
                    continue;
                }
            };

            let result = &json["result"];
            let status = result["status"].as_str().unwrap_or("unknown");

            match status {
                "complete" => {
                    // 获取下载统计信息
                    let total_length = result["totalLength"].as_str().unwrap_or("0");
                    let completed_length = result["completedLength"].as_str().unwrap_or("0");
                    let download_speed = result["downloadSpeed"].as_str().unwrap_or("0");

                    if let (Ok(total), Ok(completed), Ok(speed)) = (
                        total_length.parse::<u64>(),
                        completed_length.parse::<u64>(),
                        download_speed.parse::<u64>(),
                    ) {
                        let total_mb = total as f64 / 1_048_576.0;
                        let completed_mb = completed as f64 / 1_048_576.0;
                        let _speed_mb = speed as f64 / 1_048_576.0;
                        let duration = start_time.elapsed();

                        info!(
                            "aria2下载完成 (GID: {})，大小: {:.2}/{:.2} MB，平均速度: {:.2} MB/s，用时: {:.1}s",
                            gid,
                            completed_mb,
                            total_mb,
                            completed_mb / duration.as_secs_f64().max(0.1),
                            duration.as_secs_f64()
                        );
                    } else {
                        info!(
                            "aria2下载完成 (GID: {})，用时: {:.1}s",
                            gid,
                            start_time.elapsed().as_secs_f64()
                        );
                    }
                    return Ok(());
                }
                "error" => {
                    let error_msg = result["errorMessage"].as_str().unwrap_or("Unknown error");

                    // 检查是否是暂停导致的错误
                    if crate::task::TASK_CONTROLLER.is_paused() {
                        info!("aria2下载因用户暂停而失败 (GID: {})：{}", gid, error_msg);
                    } else {
                        error!("aria2下载失败 (GID: {})：{}", gid, error_msg);
                    }

                    bail!("下载失败: {}", error_msg);
                }
                "removed" => {
                    // 检查是否是暂停导致的移除
                    if crate::task::TASK_CONTROLLER.is_paused() {
                        info!("aria2下载因用户暂停而被移除 (GID: {})", gid);
                    } else {
                        warn!("aria2下载被移除 (GID: {})", gid);
                    }
                    bail!("下载被移除");
                }
                "active" => {
                    // 优化：动态调整检查间隔和进度监控
                    let _total_length = result["totalLength"]
                        .as_str()
                        .unwrap_or("0")
                        .parse::<u64>()
                        .unwrap_or(0);
                    let completed_length = result["completedLength"]
                        .as_str()
                        .unwrap_or("0")
                        .parse::<u64>()
                        .unwrap_or(0);
                    let download_speed = result["downloadSpeed"]
                        .as_str()
                        .unwrap_or("0")
                        .parse::<u64>()
                        .unwrap_or(0);

                    // 检查下载是否停滞
                    if completed_length == last_completed_length {
                        stall_count += 1;
                        if stall_count >= 30 {
                            // 30次检查无进度（约30-60秒）
                            warn!("下载停滞检测 (GID: {})，无进度超过{}次检查", gid, stall_count);
                            if stall_count >= 60 {
                                // 更长时间停滞则认为失败
                                bail!("下载停滞超过60次状态检查，可能网络异常");
                            }
                        }
                    } else {
                        stall_count = 0;
                        last_completed_length = completed_length;
                    }

                    // 不显示中间进度，只在完成时显示统计信息

                    // 动态调整检查间隔 - 适度增加间隔，减少RPC压力
                    check_interval = if download_speed > 5_242_880 {
                        // >5MB/s
                        Duration::from_secs(1) // 高速下载，1秒检查一次
                    } else if download_speed > 1_048_576 {
                        // >1MB/s
                        Duration::from_secs(2) // 中速下载，2秒检查一次
                    } else if download_speed > 0 {
                        Duration::from_secs(3) // 慢速下载，3秒检查一次
                    } else {
                        Duration::from_secs(5) // 无速度，5秒检查一次
                    };
                }
                "waiting" => {
                    debug!("下载等待中 (GID: {})", gid);
                    check_interval = Duration::from_secs(3); // 等待状态，3秒间隔
                }
                "paused" => {
                    debug!("下载已暂停 (GID: {})", gid);
                    check_interval = Duration::from_secs(10); // 暂停状态，10秒间隔
                }
                _ => {
                    warn!("未知下载状态 (GID: {})：{}", gid, status);
                    check_interval = Duration::from_secs(3); // 未知状态，3秒间隔
                }
            }

            // 使用动态检查间隔
            sleep(check_interval).await;
        }
    }

    /// 合并视频和音频文件
    pub async fn merge(&self, video_path: &Path, audio_path: &Path, output_path: &Path) -> Result<()> {
        use crate::downloader::Downloader;

        // 使用内置的合并功能
        let temp_downloader = Downloader::new(self.client.clone());
        temp_downloader.merge(video_path, audio_path, output_path).await
    }

    /// 优雅关闭所有aria2进程
    pub async fn shutdown(&self) -> Result<()> {
        info!("正在关闭所有aria2实例...");

        let mut instances = self.aria2_instances.lock().await;
        let mut shutdown_futures = Vec::new();

        for (i, instance) in instances.iter_mut().enumerate() {
            let rpc_port = instance.rpc_port;
            let rpc_secret = instance.rpc_secret.clone();
            let rpc_client = self.rpc_client.clone();

            // 尝试优雅关闭aria2实例
            let shutdown_future = async move {
                let url = format!("http://127.0.0.1:{}/jsonrpc", rpc_port);
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "aria2.shutdown",
                    "id": "shutdown",
                    "params": [format!("token:{}", rpc_secret)]
                });

                let _ = rpc_client.post(&url).json(&payload).send().await;
                tokio::time::sleep(Duration::from_millis(1000)).await;
            };

            shutdown_futures.push(shutdown_future);

            // 强制终止进程 - Windows兼容性改进
            if let Err(e) = instance.process.kill().await {
                warn!("终止aria2实例 {} 失败: {}", i + 1, e);

                // 如果普通kill失败，尝试使用系统命令强制终止
                #[cfg(target_os = "windows")]
                {
                    if let Some(pid) = instance.process.id() {
                        let _ = crate::utils::process::tokio_command("taskkill")
                            .args(["/F", "/PID", &pid.to_string()])
                            .output()
                            .await;
                        info!("已强制终止Windows aria2进程 PID: {}", pid);
                    }
                }

                #[cfg(target_os = "linux")]
                {
                    if let Some(pid) = instance.process.id() {
                        let _ = crate::utils::process::tokio_command("kill")
                            .args(["-9", &pid.to_string()])
                            .output()
                            .await;
                        info!("已强制终止Linux aria2进程 PID: {}", pid);
                    }
                }

                #[cfg(any(target_os = "macos", target_os = "ios"))]
                {
                    if let Some(pid) = instance.process.id() {
                        let _ = crate::utils::process::tokio_command("kill")
                            .args(["-9", &pid.to_string()])
                            .output()
                            .await;
                        info!("已强制终止macOS aria2进程 PID: {}", pid);
                    }
                }
            } else {
                debug!("aria2实例 {} 已终止", i + 1);
            }
        }

        // 等待所有优雅关闭完成
        futures::future::join_all(shutdown_futures).await;

        instances.clear();

        // 最后再次确保所有aria2进程都被清理
        tokio::time::sleep(Duration::from_millis(1000)).await;
        Self::cleanup_all_aria2_processes().await;

        info!("所有aria2实例已关闭");
        Ok(())
    }

    /// 智能健康检查调度器：只在合适时机执行健康检查
    pub async fn smart_health_check(
        client: &ReqwestClient,
        instances: &Arc<Mutex<Vec<Aria2Instance>>>,
        instance_count: usize,
    ) -> Result<()> {
        // 首先检查是否启用了健康检查
        let config = crate::config::with_config(|bundle| bundle.config.clone());
        if !config.enable_aria2_health_check {
            debug!("aria2健康检查已禁用，跳过检查");
            return Ok(());
        }

        // 检查当前是否适合进行健康检查
        let instances_guard = instances.lock().await;
        let total_load: usize = instances_guard.iter().map(|i| i.get_load()).sum();
        drop(instances_guard);

        if total_load > 0 {
            debug!("系统忙碌 (负载: {})，跳过健康检查", total_load);
            return Ok(());
        }

        debug!("系统空闲，开始执行健康检查");
        Self::health_check(client, instances, instance_count).await
    }

    /// 健康检查：移除不健康的实例并重新启动（增强版）
    pub async fn health_check(
        client: &ReqwestClient,
        instances: &Arc<Mutex<Vec<Aria2Instance>>>,
        instance_count: usize,
    ) -> Result<()> {
        let mut instances_guard = instances.lock().await;
        let mut unhealthy_indices = Vec::new();

        // 检查每个实例的健康状态，避免在忙碌时进行RPC检查
        for (i, instance) in instances_guard.iter_mut().enumerate() {
            let current_load = instance.get_load();

            // 对于有活跃下载的实例，采用更宽松的健康检查策略
            if current_load > 0 {
                debug!("实例 {} 有活跃下载 (负载: {})，跳过健康检查", i + 1, current_load);
                continue;
            }

            // 基础进程检查
            if !instance.is_healthy() {
                warn!("aria2实例 {} 进程不健康，准备重启", i + 1);
                unhealthy_indices.push(i);
                continue;
            }

            // 对于空闲实例，进行RPC健康检查
            let rpc_healthy = Self::check_instance_rpc_health(client, instance.rpc_port, &instance.rpc_secret).await;
            if !rpc_healthy {
                warn!("aria2实例 {} RPC连接不健康，准备重启", i + 1);
                unhealthy_indices.push(i);
            }
        }

        // 移除不健康的实例
        for &index in unhealthy_indices.iter().rev() {
            let removed_instance = instances_guard.remove(index);
            info!("移除不健康的aria2实例，端口: {}", removed_instance.rpc_port);
        }

        let unhealthy_count = unhealthy_indices.len();
        drop(instances_guard);

        // 检查是否启用自动重启，决定是否重新启动不健康的实例
        let config = crate::config::with_config(|bundle| bundle.config.clone());
        if unhealthy_count > 0 {
            if config.enable_aria2_auto_restart {
                info!("重新启动 {} 个aria2实例", unhealthy_count);

                for i in 0..unhealthy_count {
                    match Self::create_missing_instance(instances).await {
                        Ok(instance) => {
                            instances.lock().await.push(instance);
                            info!("成功重启第{}个aria2实例", i + 1);
                        }
                        Err(e) => {
                            error!("重启第{}个aria2实例失败: {:#}", i + 1, e);
                            // 记录详细的失败原因以便诊断
                            Self::log_startup_diagnostics().await;
                        }
                    }
                }
            } else {
                warn!("检测到 {} 个不健康的aria2实例，但自动重启已禁用", unhealthy_count);
            }
        }

        let current_count = instances.lock().await.len();
        if current_count == 0 {
            bail!("所有aria2实例都不可用");
        }

        debug!("健康检查完成，当前可用实例数: {}/{}", current_count, instance_count);
        Ok(())
    }

    /// 检查实例的RPC健康状态
    async fn check_instance_rpc_health(client: &ReqwestClient, rpc_port: u16, rpc_secret: &str) -> bool {
        let client_clone = client.clone();
        let rpc_secret_clone = rpc_secret.to_string();

        let result = Self::retry_with_backoff_static(
            client,
            "RPC健康检查",
            2, // 只重试2次
            Duration::from_millis(500),
            Duration::from_secs(10), // 增加RPC健康检查超时时间到10秒
            move || {
                let client = client_clone.clone();
                let rpc_secret = rpc_secret_clone.clone();
                async move {
                    let url = format!("http://127.0.0.1:{}/jsonrpc", rpc_port);
                    let payload = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "aria2.getVersion",
                        "id": "health_check",
                        "params": [format!("token:{}", rpc_secret)]
                    });

                    let response = client.post(&url).json(&payload).send().await?;
                    if response.status().is_success() {
                        Ok(())
                    } else {
                        bail!("RPC返回错误状态: {}", response.status())
                    }
                }
            },
        )
        .await;

        result.is_ok()
    }

    /// 创建缺失的实例（用于监控任务自动恢复）
    async fn create_missing_instance(_instances: &Arc<Mutex<Vec<Aria2Instance>>>) -> Result<Aria2Instance> {
        let rpc_port = Self::find_available_port().await?;
        let rpc_secret = Self::generate_secret();

        info!("尝试创建缺失的aria2实例，端口: {}", rpc_port);

        // 需要临时创建一个aria2下载器来启动实例
        let aria2_binary_path = Self::extract_aria2_binary().await?;
        let temp_downloader = Self::create_temp_downloader(aria2_binary_path).await?;

        let process = temp_downloader.start_single_instance(rpc_port, &rpc_secret).await?;
        let instance = Aria2Instance::new(process, rpc_port, rpc_secret.clone());

        // 等待RPC服务启动
        info!("等待新aria2实例RPC服务启动...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // 验证连接
        temp_downloader.test_instance_connection(rpc_port, &rpc_secret).await?;

        info!("新aria2实例创建成功，端口: {}", rpc_port);
        Ok(instance)
    }

    /// 创建临时下载器用于实例恢复
    async fn create_temp_downloader(aria2_binary_path: PathBuf) -> Result<Self> {
        let client = crate::bilibili::Client::new();
        Ok(Self {
            client,
            rpc_client: Self::build_rpc_client()?,
            aria2_instances: Arc::new(Mutex::new(Vec::new())),
            aria2_binary_path,
            instance_count: 1,
        })
    }

    /// 记录启动诊断信息
    async fn log_startup_diagnostics() {
        error!("=== Aria2启动失败诊断信息 ===");

        // 检查aria2二进制文件
        match Self::extract_aria2_binary().await {
            Ok(binary_path) => {
                if binary_path.exists() {
                    info!("✓ aria2二进制文件存在: {}", binary_path.display());

                    // 检查文件权限
                    match tokio::fs::metadata(&binary_path).await {
                        Ok(metadata) => {
                            info!("✓ 文件大小: {} 字节", metadata.len());
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let perms = metadata.permissions();
                                info!("✓ 文件权限: {:o}", perms.mode());
                            }
                        }
                        Err(e) => error!("✗ 无法读取文件元数据: {}", e),
                    }

                    // 测试执行
                    match crate::utils::process::tokio_command(&binary_path)
                        .arg("--version")
                        .output()
                        .await
                    {
                        Ok(output) => {
                            if output.status.success() {
                                let version = String::from_utf8_lossy(&output.stdout);
                                info!("✓ aria2版本: {}", version.trim());
                            } else {
                                error!("✗ aria2执行失败，退出码: {:?}", output.status.code());
                                error!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                            }
                        }
                        Err(e) => error!("✗ 无法执行aria2: {}", e),
                    }
                } else {
                    error!("✗ aria2二进制文件不存在: {}", binary_path.display());
                }
            }
            Err(e) => error!("✗ 无法获取aria2二进制文件: {:#}", e),
        }

        // 检查端口可用性
        match Self::find_available_port().await {
            Ok(port) => info!("✓ 找到可用端口: {}", port),
            Err(e) => error!("✗ 无法找到可用端口: {}", e),
        }

        // 检查系统资源
        #[cfg(target_os = "linux")]
        {
            // 检查进程限制
            if let Ok(output) = crate::utils::process::tokio_command("ulimit")
                .args(["-n"])
                .output()
                .await
            {
                if output.status.success() {
                    let limit = String::from_utf8_lossy(&output.stdout);
                    info!("✓ 文件描述符限制: {}", limit.trim());
                }
            }

            // 检查内存使用
            if let Ok(output) = crate::utils::process::tokio_command("free").args(["-h"]).output().await {
                if output.status.success() {
                    let memory_info = String::from_utf8_lossy(&output.stdout);
                    info!("✓ 内存状态:\n{}", memory_info);
                }
            }
        }

        error!("=== 诊断信息结束 ===");
    }

    /// 通用的重试机制，支持指数退避（优化版）
    async fn retry_with_backoff<F, Fut, T>(
        &self,
        operation_name: &str,
        max_retries: u32,
        initial_delay: Duration,
        timeout_duration: Duration,
        operation: F,
    ) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut delay = initial_delay;

        for attempt in 1..=max_retries {
            match timeout(timeout_duration, operation()).await {
                Ok(Ok(result)) => {
                    if attempt > 1 {
                        debug!("{}在第{}次尝试后成功", operation_name, attempt);
                    }
                    return Ok(result);
                }
                Ok(Err(e)) => {
                    if attempt == max_retries {
                        // 只有在最终失败时才记录warn级别日志
                        if operation_name == "状态检查" || operation_name == "RPC健康检查" {
                            debug!("{}在{}次尝试后最终失败: {:#}", operation_name, max_retries, e);
                        } else {
                            warn!("{}在{}次尝试后最终失败: {:#}", operation_name, max_retries, e);
                        }
                        return Err(e);
                    }

                    // 优化：根据错误类型调整重试策略
                    let should_retry_immediately =
                        e.to_string().contains("Connection refused") || e.to_string().contains("timeout");

                    if should_retry_immediately && attempt <= 2 {
                        debug!(
                            "{}第{}次尝试失败（网络问题）: {:#}，立即重试",
                            operation_name, attempt, e
                        );
                        // 网络问题立即重试，不等待
                        continue;
                    } else {
                        debug!(
                            "{}第{}次尝试失败: {:#}，{}ms后重试",
                            operation_name,
                            attempt,
                            e,
                            delay.as_millis()
                        );
                    }
                }
                Err(_) => {
                    let timeout_err = anyhow::anyhow!("{}超时({:?})", operation_name, timeout_duration);
                    if attempt == max_retries {
                        // 状态检查和RPC健康检查超时降级为debug日志，其他关键操作仍为warn
                        if operation_name == "状态检查" || operation_name == "RPC健康检查" {
                            debug!("{}在{}次尝试后最终超时", operation_name, max_retries);
                        } else {
                            warn!("{}在{}次尝试后最终超时", operation_name, max_retries);
                        }
                        return Err(timeout_err);
                    }
                    // 所有中间超时都使用debug级别，避免日志噪音
                    debug!(
                        "{}第{}次尝试超时，{}ms后重试",
                        operation_name,
                        attempt,
                        delay.as_millis()
                    );
                }
            }

            sleep(delay).await;
            delay = std::cmp::min(delay * 2, Duration::from_secs(10)); // 减少最大延迟到10秒
        }

        unreachable!()
    }

    /// 静态版本的重试方法，用于健康检查
    async fn retry_with_backoff_static<F, Fut, T>(
        _client: &ReqwestClient,
        operation_name: &str,
        max_retries: u32,
        initial_delay: Duration,
        timeout_duration: Duration,
        operation: F,
    ) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut delay = initial_delay;

        for attempt in 1..=max_retries {
            match timeout(timeout_duration, operation()).await {
                Ok(Ok(result)) => {
                    if attempt > 1 {
                        debug!("{}在第{}次尝试后成功", operation_name, attempt);
                    }
                    return Ok(result);
                }
                Ok(Err(e)) => {
                    if attempt == max_retries {
                        debug!("{}在{}次尝试后最终失败: {:#}", operation_name, max_retries, e);
                        return Err(e);
                    }

                    let should_retry_immediately =
                        e.to_string().contains("Connection refused") || e.to_string().contains("timeout");

                    if should_retry_immediately && attempt <= 2 {
                        debug!(
                            "{}第{}次尝试失败（网络问题）: {:#}，立即重试",
                            operation_name, attempt, e
                        );
                        continue;
                    } else {
                        debug!(
                            "{}第{}次尝试失败: {:#}，{}ms后重试",
                            operation_name,
                            attempt,
                            e,
                            delay.as_millis()
                        );
                    }
                }
                Err(_) => {
                    let timeout_err = anyhow::anyhow!("{}超时({:?})", operation_name, timeout_duration);
                    if attempt == max_retries {
                        debug!("{}在{}次尝试后最终超时", operation_name, max_retries);
                        return Err(timeout_err);
                    }
                    debug!(
                        "{}第{}次尝试超时，{}ms后重试",
                        operation_name,
                        attempt,
                        delay.as_millis()
                    );
                }
            }

            sleep(delay).await;
            delay = std::cmp::min(delay * 2, Duration::from_secs(10));
        }

        unreachable!()
    }

    /// 增强的文件验证逻辑
    async fn verify_downloaded_file(&self, path: &Path) -> Result<()> {
        // 基本存在性检查
        if !path.exists() {
            bail!("Download completed but file not found: {}", path.display());
        }

        // 等待文件系统同步
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 文件大小检查
        match tokio::fs::metadata(path).await {
            Ok(metadata) => {
                let file_size = metadata.len();
                if file_size == 0 {
                    warn!("下载的文件大小为0: {}", path.display());
                    bail!("Downloaded file is empty: {}", path.display());
                }
                debug!("文件下载验证成功: {} (大小: {} 字节)", path.display(), file_size);
                Ok(())
            }
            Err(e) => {
                error!("无法读取下载文件的元数据: {} - {}", path.display(), e);
                bail!("Cannot read downloaded file metadata: {} - {}", path.display(), e);
            }
        }
    }
}

impl Drop for Aria2Downloader {
    fn drop(&mut self) {
        // 保留aria2二进制文件，避免在暂停/重启扫描时被删除
        // 如果后续需要清理，可在显式卸载/清理路径中处理，而不是在Drop中
    }
}

#[cfg(test)]
mod tests {
    use super::{
        aria2_download_failure_message, next_aria2_source, plan_aria2_download_sources, resolve_aria2_download_url,
        Aria2DownloadSource, Aria2Package, ARIA2_MIRROR_BASE_ENV,
    };

    const ARCHIVE: &str = "aria2-x86_64-linux-musl_static.zip";
    const PRIMARY_R2D: &str = "https://r2d.dnof.net/files/tools/aria2/aria2-x86_64-linux-musl_static.zip";
    const FALLBACK_GITHUB: &str =
        "https://github.com/NeeYoonc/bili-sync-up/releases/download/tools-aria2/aria2-x86_64-linux-musl_static.zip";
    const SHA256: &str = "0d86b4fe16bc820f9e97ccd1034c88db87b884ed59b8db3ba7ebf286424389f4";

    const TEST_PACKAGE: Aria2Package = Aria2Package {
        target_key: "linux-x86_64",
        archive_name: ARCHIVE,
        binary_name: "aria2c",
        primary_url: PRIMARY_R2D,
        fallback_url: FALLBACK_GITHUB,
        sha256: SHA256,
    };

    fn labels(sources: &[Aria2DownloadSource]) -> Vec<&'static str> {
        sources.iter().map(|source| source.label).collect()
    }

    /// 默认先走 r2d 公开主源，失败才用 GitHub Release 备用源；两个源是同一个文件，sha256 相同。
    #[test]
    fn aria2_default_sources_try_primary_then_fallback_with_same_sha256() {
        let sources = plan_aria2_download_sources(&TEST_PACKAGE, None);
        assert_eq!(labels(&sources), ["公开镜像(r2d.dnof.net)", "GitHub Release(备用源)"]);
        assert_eq!(sources[0].url, PRIMARY_R2D);
        assert_eq!(sources[1].url, FALLBACK_GITHUB);
        assert_eq!(sources[0].sha256, SHA256);
        assert_eq!(sources[1].sha256, SHA256);
    }

    /// 主源失败后应回退到备用源；备用源也失败就没有更多源了。
    #[test]
    fn aria2_falls_back_to_next_source_after_failure() {
        let sources = plan_aria2_download_sources(&TEST_PACKAGE, None);
        let fallback = next_aria2_source(&sources, 0).expect("主源失败后应回退到备用源");
        assert_eq!(fallback.url, FALLBACK_GITHUB);
        assert!(next_aria2_source(&sources, 1).is_none());
    }

    /// 配置镜像基址时只走镜像（单源、不回退），环境变量优先级最高。
    #[test]
    fn aria2_mirror_base_overrides_builtin_sources() {
        let sources = plan_aria2_download_sources(&TEST_PACKAGE, Some(" https://mirror.example.com/aria2/ "));
        assert_eq!(labels(&sources), ["镜像"]);
        assert_eq!(
            sources[0].url,
            "https://mirror.example.com/aria2/aria2-x86_64-linux-musl_static.zip"
        );
        assert_eq!(sources[0].sha256, SHA256);
        assert!(next_aria2_source(&sources, 0).is_none());
    }

    /// 空白 / 纯斜杠的镜像基址视为未配置，回退主源；尾斜杠与首尾空白会被清理。
    #[test]
    fn aria2_mirror_base_is_normalized() {
        for mirror in [None, Some(""), Some("   "), Some("/"), Some("///")] {
            let sources = plan_aria2_download_sources(&TEST_PACKAGE, mirror);
            assert_eq!(sources[0].url, PRIMARY_R2D);
            assert_eq!(resolve_aria2_download_url(PRIMARY_R2D, ARCHIVE, mirror), PRIMARY_R2D);
        }
        assert_eq!(
            resolve_aria2_download_url(PRIMARY_R2D, ARCHIVE, Some("  https://mirror.example.com/aria2//  ")),
            "https://mirror.example.com/aria2/aria2-x86_64-linux-musl_static.zip"
        );
    }

    /// 两个源都失败时，错误信息要能看出试过哪些源、各自失败原因，以及手动放置 / 镜像提示。
    #[test]
    fn aria2_all_sources_failed_message_contains_actionable_hints() {
        let failures = vec![
            ("公开镜像(r2d.dnof.net)".to_string(), "network unreachable".to_string()),
            ("GitHub Release(备用源)".to_string(), "校验不通过".to_string()),
        ];
        let message = aria2_download_failure_message(
            &failures,
            &format!("可放在 /config/tools/aria2 或设置 {} 指向镜像", ARIA2_MIRROR_BASE_ENV),
        );
        assert!(message.contains("已尝试 2 个源"), "{message}");
        assert!(
            message.contains("公开镜像(r2d.dnof.net): network unreachable"),
            "{message}"
        );
        assert!(message.contains("GitHub Release(备用源): 校验不通过"), "{message}");
        assert!(message.contains("可放在 /config/tools/aria2"), "{message}");
        assert!(message.contains(ARIA2_MIRROR_BASE_ENV), "{message}");
    }

    /// 环境变量名与文档、日志提示保持一致。
    #[test]
    fn aria2_mirror_env_name_matches_documentation() {
        assert_eq!(ARIA2_MIRROR_BASE_ENV, "BILI_SYNC_ARIA2_MIRROR_BASE");
    }
}
