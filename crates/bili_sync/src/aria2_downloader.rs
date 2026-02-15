use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use reqwest::Client as ReqwestClient;
use reqwest::Method;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use crate::bilibili::Client;
use crate::config::CONFIG_DIR;
use crate::http::headers::{create_api_headers, create_aria2_headers};

/// 嵌入的aria2二进制文件 (编译时自动下载对应平台版本)
#[cfg(target_os = "windows")]
static ARIA2_BINARY_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/aria2c.exe.gz"));

#[cfg(target_os = "linux")]
static ARIA2_BINARY_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/aria2c.gz"));

#[cfg(any(target_os = "macos", target_os = "ios"))]
static ARIA2_BINARY_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/aria2c.gz"));

static ARIA2_BINARY_DECOMPRESSED: OnceLock<Vec<u8>> = OnceLock::new();

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
            let output = tokio::process::Command::new("taskkill")
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
            let output = tokio::process::Command::new("pkill")
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
            let output = tokio::process::Command::new("pkill")
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
    async fn try_get_file_size(&self, url: &str) -> Option<u64> {
        let result = timeout(Duration::from_secs(5), async {
            let mut headers = create_api_headers();
            if let Ok(range) = "bytes=0-0".parse() {
                headers.insert("Range", range);
            }

            self.client
                .request(Method::GET, url, None)
                .headers(headers)
                .send()
                .await
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

    async fn get_embedded_aria2_binary() -> Result<&'static Vec<u8>> {
        if let Some(bytes) = ARIA2_BINARY_DECOMPRESSED.get() {
            return Ok(bytes);
        }

        let bytes = tokio::task::spawn_blocking(|| -> Result<Vec<u8>> {
            let mut decoder = GzDecoder::new(ARIA2_BINARY_GZ);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).context("解压内置 aria2 二进制失败")?;
            Ok(out)
        })
        .await
        .context("解压内置 aria2 二进制失败")??;

        Ok(ARIA2_BINARY_DECOMPRESSED.get_or_init(|| bytes))
    }

    /// 提取嵌入的aria2二进制文件到临时目录，失败时回退到系统aria2
    async fn extract_aria2_binary() -> Result<PathBuf> {
        // 使用配置文件夹存储aria2二进制文件，而不是临时目录
        let binary_name = if cfg!(target_os = "windows") {
            "aria2c.exe"
        } else {
            "aria2c"
        };
        let binary_path = CONFIG_DIR.join(binary_name);

        // 确保配置目录存在
        if let Err(e) = tokio::fs::create_dir_all(&*CONFIG_DIR).await {
            warn!("创建配置目录失败: {}, 将使用临时目录", e);
            let temp_dir = std::env::temp_dir();
            return Self::extract_aria2_binary_to_temp(temp_dir, binary_name).await;
        }

        // 如果文件已存在且可执行，直接返回
        if binary_path.exists() {
            // 验证文件是否为有效的aria2可执行文件
            if Self::is_valid_aria2_binary(&binary_path).await {
                return Ok(binary_path);
            } else {
                // 如果是无效的文件（如占位文件），删除它
                let _ = tokio::fs::remove_file(&binary_path).await;
            }
        }

        // 尝试写入嵌入的二进制文件（gzip 压缩内嵌，运行时解压写出）
        debug!("尝试提取aria2二进制文件到配置目录: {}", binary_path.display());
        let embedded_bytes = Self::get_embedded_aria2_binary().await?;
        match tokio::fs::write(&binary_path, embedded_bytes).await {
            Ok(_) => {
                debug!("aria2二进制文件写入配置目录成功，大小: {} bytes", embedded_bytes.len());

                // 在Unix系统上设置执行权限
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = tokio::fs::metadata(&binary_path).await {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o755);
                        match tokio::fs::set_permissions(&binary_path, perms).await {
                            Ok(_) => {
                                debug!("已设置aria2二进制文件执行权限");
                            }
                            Err(e) => {
                                warn!("无法设置aria2执行权限: {}，可能是挂载目录权限限制，将尝试临时目录", e);
                                // 权限设置失败，删除文件并尝试其他位置
                                let _ = tokio::fs::remove_file(&binary_path).await;
                                // 尝试使用临时目录
                                let temp_dir = std::env::temp_dir();
                                return Self::extract_aria2_binary_to_temp(temp_dir, binary_name).await;
                            }
                        }
                    }
                }

                // 验证提取的文件是否有效
                debug!("开始验证提取到配置目录的aria2二进制文件...");
                if Self::is_valid_aria2_binary(&binary_path).await {
                    info!("aria2二进制文件已提取到配置目录: {}", binary_path.display());
                    return Ok(binary_path);
                } else {
                    warn!("配置目录中的aria2二进制文件无效，尝试使用系统aria2");
                    let _ = tokio::fs::remove_file(&binary_path).await;
                }
            }
            Err(e) => {
                warn!("提取aria2二进制文件到配置目录失败: {}, 尝试使用系统aria2", e);
            }
        }

        // 回退到系统安装的aria2
        Self::find_system_aria2().await
    }

    /// 备用方案：提取到临时目录
    async fn extract_aria2_binary_to_temp(temp_dir: PathBuf, binary_name: &str) -> Result<PathBuf> {
        let binary_path = temp_dir.join(format!("bili-sync-{}", binary_name));

        debug!("尝试提取aria2二进制文件到临时目录: {}", binary_path.display());

        // 如果文件已存在且可执行，直接返回
        if binary_path.exists() {
            if Self::is_valid_aria2_binary(&binary_path).await {
                return Ok(binary_path);
            } else {
                let _ = tokio::fs::remove_file(&binary_path).await;
            }
        }

        // 尝试写入嵌入的二进制文件（gzip 压缩内嵌，运行时解压写出）
        let embedded_bytes = Self::get_embedded_aria2_binary().await?;
        match tokio::fs::write(&binary_path, embedded_bytes).await {
            Ok(_) => {
                debug!("aria2二进制文件写入临时目录成功，大小: {} bytes", embedded_bytes.len());

                // 在Unix系统上设置执行权限
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = tokio::fs::metadata(&binary_path).await {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o755);
                        if let Err(e) = tokio::fs::set_permissions(&binary_path, perms).await {
                            warn!("临时目录中无法设置aria2执行权限: {}，将回退到系统aria2", e);
                            let _ = tokio::fs::remove_file(&binary_path).await;
                            return Self::find_system_aria2().await;
                        } else {
                            debug!("已设置临时目录中aria2二进制文件执行权限");
                        }
                    }
                }

                // 验证提取的文件是否有效
                if Self::is_valid_aria2_binary(&binary_path).await {
                    info!("aria2二进制文件已提取到临时目录: {}", binary_path.display());
                    return Ok(binary_path);
                } else {
                    warn!("临时目录中的aria2二进制文件无效");
                    let _ = tokio::fs::remove_file(&binary_path).await;
                }
            }
            Err(e) => {
                warn!("提取aria2二进制文件到临时目录失败: {}", e);
            }
        }

        // 最终回退到系统安装的aria2
        Self::find_system_aria2().await
    }

    /// 验证aria2二进制文件是否有效
    async fn is_valid_aria2_binary(path: &Path) -> bool {
        if !path.exists() {
            warn!("aria2二进制文件不存在: {}", path.display());
            return false;
        }

        // 尝试执行 aria2c --version 来验证
        match tokio::process::Command::new(path).arg("--version").output().await {
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
        match tokio::process::Command::new("which").arg("aria2c").output().await {
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
            match tokio::process::Command::new("where").arg("aria2c").output().await {
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
        let mut child = tokio::process::Command::new(&self.aria2_binary_path)
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
            .add_download_task_to_instance(urls, dir, file_name, rpc_port, &rpc_secret)
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
        rpc_port: u16,
        rpc_secret: &str,
    ) -> Result<String> {
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
        let threads = if let Some(file_size_bytes) = self.try_get_file_size(urls[0]).await {
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
            "header": create_aria2_headers()
        });

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
                        let _ = tokio::process::Command::new("taskkill")
                            .args(["/F", "/PID", &pid.to_string()])
                            .output()
                            .await;
                        info!("已强制终止Windows aria2进程 PID: {}", pid);
                    }
                }

                #[cfg(target_os = "linux")]
                {
                    if let Some(pid) = instance.process.id() {
                        let _ = tokio::process::Command::new("kill")
                            .args(["-9", &pid.to_string()])
                            .output()
                            .await;
                        info!("已强制终止Linux aria2进程 PID: {}", pid);
                    }
                }

                #[cfg(any(target_os = "macos", target_os = "ios"))]
                {
                    if let Some(pid) = instance.process.id() {
                        let _ = tokio::process::Command::new("kill")
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
                    match tokio::process::Command::new(&binary_path)
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
            if let Ok(output) = tokio::process::Command::new("ulimit").args(["-n"]).output().await {
                if output.status.success() {
                    let limit = String::from_utf8_lossy(&output.stdout);
                    info!("✓ 文件描述符限制: {}", limit.trim());
                }
            }

            // 检查内存使用
            if let Ok(output) = tokio::process::Command::new("free").args(["-h"]).output().await {
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
