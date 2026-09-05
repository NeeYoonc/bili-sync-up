//! TikTok 官方接口的 Chrome TLS 指纹模拟请求（curl-impersonate 子进程）。
//!
//! TikTok 的 Akamai 风控会按 TLS/JA3/JA4 与 HTTP/2 指纹识别非浏览器客户端。
//! 服务端用 reqwest（rustls/OpenSSL）重放“完整签名 + 登录 Cookie”也会被判定为
//! 非浏览器，返回 HTTP 200 空 body / 空列表；只有 Chrome 指纹（BoringSSL）能过。
//! 这里按 yt-dlp 自动安装的同样模式，首次使用时自动下载 curl-impersonate 的
//! 预编译二进制（lexiforest fork，含 Windows/linux-musl 全平台静态包），并封装
//! 一个 GET 请求函数供 TikTok 关注/我的喜欢等官方接口使用。
//!
//! 实测：curl-impersonate `--impersonate chrome131` + Node webmssdk 签名 URL +
//! 登录 Cookie，可正常拉取关注列表（userList=31）；同参数换 reqwest 则空响应。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::process::Command;
use tracing::{info, warn};

use crate::config::CONFIG_DIR;
use crate::youtube::configured_external_proxy;

/// curl-impersonate 版本（lexiforest fork 的 release tag）。
const CURL_IMPERSONATE_VERSION: &str = "1.5.6";
const CURL_IMPERSONATE_RELEASE_BASE: &str =
    "https://github.com/lexiforest/curl-impersonate/releases/download";
/// Mozilla CA 证书包（Windows 的 BoringSSL 构建不带系统证书库，必须显式指定）。
const CACERT_DOWNLOAD_URL: &str = "https://curl.se/ca/cacert.pem";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(240);
/// 单个官方接口请求超时。
pub(crate) const TIKTOK_IMPERSONATE_TIMEOUT: Duration = Duration::from_secs(45);
/// 写入命令 stdout 的 HTTP 状态码标记。
const STATUS_MARKER: &str = "\n__BILI_SYNC_HTTP__";

struct CurlImpersonatePackage {
    target_key: &'static str,
    asset_name: &'static str,
    binary_name: &'static str,
    /// Windows 包需要额外下载 CA 证书（BoringSSL 无系统证书库）。
    need_cacert: bool,
}

fn current_package() -> Option<CurlImpersonatePackage> {
    let pkg = |target_key, asset_name, binary_name, need_cacert| CurlImpersonatePackage {
        target_key,
        asset_name,
        binary_name,
        need_cacert,
    };
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some(pkg(
            "windows-x86_64",
            "libcurl-impersonate-v1.5.6.x86_64-win32.tar.gz",
            "curl-impersonate.exe",
            true,
        )),
        ("windows", "aarch64") => Some(pkg(
            "windows-aarch64",
            "libcurl-impersonate-v1.5.6.arm64-win32.tar.gz",
            "curl-impersonate.exe",
            true,
        )),
        ("linux", "x86_64") => Some(pkg(
            "linux-x86_64",
            "curl-impersonate-v1.5.6.x86_64-linux-musl.tar.gz",
            "curl-impersonate",
            false,
        )),
        ("linux", "aarch64") => Some(pkg(
            "linux-aarch64",
            "curl-impersonate-v1.5.6.aarch64-linux-musl.tar.gz",
            "curl-impersonate",
            false,
        )),
        ("macos", "x86_64") => Some(pkg(
            "macos-x86_64",
            "curl-impersonate-v1.5.6.x86_64-macos.tar.gz",
            "curl-impersonate",
            false,
        )),
        ("macos", "aarch64") => Some(pkg(
            "macos-aarch64",
            "curl-impersonate-v1.5.6.arm64-macos.tar.gz",
            "curl-impersonate",
            false,
        )),
        _ => None,
    }
}

fn install_root(package: &CurlImpersonatePackage) -> PathBuf {
    CONFIG_DIR
        .join("tools")
        .join("curl-impersonate")
        .join(package.target_key)
}

fn binary_path_for(package: &CurlImpersonatePackage) -> PathBuf {
    install_root(package).join(package.binary_name)
}

fn cacert_path_for(package: &CurlImpersonatePackage) -> PathBuf {
    install_root(package).join("cacert.pem")
}

/// 找到可用的 curl-impersonate 主程序路径：
/// 1. 环境变量 BILI_SYNC_CURL_IMPERSONATE_PATH（可指向二进制或目录）；
/// 2. 自动安装的托管路径（不存在时自动下载安装）。
async fn ensure_tiktok_impersonate() -> Result<PathBuf> {
    if let Ok(configured) = std::env::var("BILI_SYNC_CURL_IMPERSONATE_PATH") {
        let path = PathBuf::from(configured);
        let binary_name = if cfg!(windows) { "curl-impersonate.exe" } else { "curl-impersonate" };
        let candidate = if path.is_dir() { path.join(binary_name) } else { path };
        if candidate.is_file() {
            return Ok(candidate);
        }
        bail!("BILI_SYNC_CURL_IMPERSONATE_PATH 指向的 curl-impersonate 不可用：{}", candidate.display());
    }
    let package = current_package().ok_or_else(|| {
        anyhow!(
            "当前系统架构暂不支持自动下载 curl-impersonate（{}-{}），请手动安装后设置 BILI_SYNC_CURL_IMPERSONATE_PATH",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let binary_path = binary_path_for(&package);
    if binary_path.is_file() {
        return Ok(binary_path);
    }
    let _guard = install_lock().lock().await;
    if binary_path.is_file() {
        return Ok(binary_path);
    }
    download_and_install(&package, &binary_path).await?;
    Ok(binary_path)
}

static INSTALL_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    INSTALL_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 下载并安装 curl-impersonate（tar.gz 解压到托管目录，Windows 额外下载 CA 证书）。
async fn download_and_install(package: &CurlImpersonatePackage, binary_path: &Path) -> Result<()> {
    let install_dir = install_root(package);
    let staging_dir = install_dir.with_extension("download");
    for dir in [&staging_dir, &install_dir] {
        if tokio::fs::try_exists(dir).await? {
            tokio::fs::remove_dir_all(dir)
                .await
                .with_context(|| format!("清理 curl-impersonate 旧目录失败：{}", dir.display()))?;
        }
    }
    tokio::fs::create_dir_all(&install_dir)
        .await
        .with_context(|| format!("创建 curl-impersonate 安装目录失败：{}", install_dir.display()))?;
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .with_context(|| format!("创建 curl-impersonate 临时目录失败：{}", staging_dir.display()))?;

    let asset_url = format!(
        "{CURL_IMPERSONATE_RELEASE_BASE}/v{CURL_IMPERSONATE_VERSION}/{}",
        package.asset_name
    );
    info!(url = %asset_url, target = package.target_key, "开始下载 curl-impersonate");
    let bytes = download_bytes(&asset_url).await?;
    let staging_for_extract = staging_dir.clone();
    let extract_result = tokio::task::spawn_blocking(move || extract_tar_gz(&bytes, &staging_for_extract))
        .await
        .context("解压 curl-impersonate 子任务失败")?;
    if let Err(error) = extract_result {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(error);
    }

    // 把包内主程序移动到安装根目录（Windows 包主程序在 bin/ 子目录）。
    let staged_binary = locate_binary_in(&staging_dir, package.binary_name)
        .ok_or_else(|| anyhow!("curl-impersonate 压缩包中未找到主程序：{}", package.binary_name))?;
    tokio::fs::rename(&staged_binary, binary_path)
        .await
        .with_context(|| format!("安装 curl-impersonate 主程序失败：{}", binary_path.display()))?;
    // Windows 包内附带的 DLL（libcurl-impersonate.dll / zlib.dll 等）一并保留，
    // 否则主程序启动会因缺少依赖而失败。
    if let Some(bin_dir) = staged_binary.parent() {
        let mut entries = tokio::fs::read_dir(bin_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.ends_with(".dll") {
                let target = install_dir.join(&file_name);
                tokio::fs::copy(entry.path(), &target)
                    .await
                    .with_context(|| format!("复制 curl-impersonate 依赖 DLL 失败：{}", name))?;
            }
        }
    }

    set_executable_permissions(binary_path).await?;
    if !impersonate_binary_usable(binary_path).await {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        let _ = tokio::fs::remove_dir_all(&install_dir).await;
        bail!("下载的 curl-impersonate 无法执行：{}", binary_path.display());
    }

    // Windows 的 BoringSSL 无系统证书库，必须下载 Mozilla CA 包才能验证 TLS。
    if package.need_cacert {
        let cacert_path = cacert_path_for(package);
        info!(path = %cacert_path.display(), "下载 curl-impersonate 使用的 CA 证书包");
        let cacert_bytes = download_bytes(CACERT_DOWNLOAD_URL).await?;
        tokio::fs::write(&cacert_path, cacert_bytes)
            .await
            .with_context(|| format!("写入 CA 证书包失败：{}", cacert_path.display()))?;
    }

    // 清理解压临时目录。
    tokio::fs::remove_dir_all(&staging_dir)
        .await
        .with_context(|| format!("清理 curl-impersonate 临时目录失败：{}", staging_dir.display()))?;
    info!(path = %binary_path.display(), "curl-impersonate 已自动安装并可用");
    Ok(())
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("bili-sync-up/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("创建下载客户端失败")?;
    let bytes = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("下载失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("下载返回非成功状态：{url}"))?
        .bytes()
        .await
        .with_context(|| format!("读取下载内容失败：{url}"))?;
    Ok(bytes.to_vec())
}

/// 解压 tar.gz 到目标目录，只处理普通文件与目录（跳过符号链接等特殊条目）。
fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<()> {
    use std::io::Read;
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .context("解析 curl-impersonate tar.gz 失败")?;
    for entry in entries {
        let mut entry = entry.context("读取 curl-impersonate tar.gz 条目失败")?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            continue;
        }
        let path = entry.path().context("解析 tar.gz 条目路径失败")?.into_owned();
        let output = dest.join(&path);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut contents = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut contents)?;
        std::fs::write(&output, contents)?;
    }
    Ok(())
}

fn locate_binary_in(root: &Path, binary_name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().map(|name| name == binary_name).unwrap_or(false) {
                return Some(path);
            }
        }
    }
    None
}

async fn set_executable_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("读取 curl-impersonate 文件权限失败：{}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(path, permissions)
            .await
            .with_context(|| format!("设置 curl-impersonate 可执行权限失败：{}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

async fn impersonate_binary_usable(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 使用 curl-impersonate（Chrome TLS 指纹）发起 GET，返回
/// (HTTP 状态码, 响应体, 响应头)。响应头以 (小写名称, 值) 列表返回，
/// 供调用方提取 TikTok 运行时续约信号（x-ms-token / Set-Cookie 中的 msToken）。
///
/// 所有 TikTok 官方敏感接口都应走这个入口；普通第三方接口仍用 reqwest。
pub(crate) async fn tiktok_impersonated_get(
    url: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<(u16, Vec<u8>, Vec<(String, String)>)> {
    // 唯一临时头文件：并发调用互不覆盖。
    static HEADER_DUMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let header_dump = std::env::temp_dir().join(format!(
        "bili-sync-curl-headers-{}-{}.txt",
        std::process::id(),
        HEADER_DUMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let binary = ensure_tiktok_impersonate().await?;
    let mut command = Command::new(&binary);
    command
        .arg("--impersonate")
        .arg("chrome131")
        .arg("-s")
        .arg("--compressed")
        .arg("--max-time")
        .arg(timeout.as_secs().to_string())
        .arg("--write-out")
        .arg(format!("{STATUS_MARKER}%{{http_code}}"))
        .arg("--dump-header")
        .arg(&header_dump)
        .kill_on_drop(true);

    // CA 证书：优先随二进制安装的 cacert.pem（Windows 必须），其次环境变量指定，
    // Linux 无自带 CA 时回退系统默认路径。
    let package = current_package();
    let mut cacert_used = false;
    if let Some(package) = &package {
        let managed_cacert = cacert_path_for(package);
        if managed_cacert.is_file() {
            command.arg("--cacert").arg(&managed_cacert);
            cacert_used = true;
        }
    }
    if !cacert_used {
        if let Ok(configured) = std::env::var("BILI_SYNC_CURL_IMPERSONATE_CACERT") {
            let configured = PathBuf::from(configured);
            if configured.is_file() {
                command.arg("--cacert").arg(&configured);
                cacert_used = true;
            }
        }
    }
    if !cacert_used {
        #[cfg(target_os = "linux")]
        if Path::new("/etc/ssl/certs/ca-certificates.crt").is_file() {
            command.arg("--cacert").arg("/etc/ssl/certs/ca-certificates.crt");
            cacert_used = true;
        }
        if !cacert_used {
            warn!("未找到 curl-impersonate 可用的 CA 证书，TikTok 请求可能因 TLS 校验失败");
        }
    }

    let proxy = configured_external_proxy();
    if !proxy.is_empty() {
        command.arg("--proxy").arg(&proxy);
    }
    for (name, value) in headers {
        if let Ok(header_line) = format_header(name, value) {
            command.arg("-H").arg(header_line);
        }
    }
    command.arg(url);

    let output = tokio::time::timeout(timeout + Duration::from_secs(10), command.output())
        .await
        .map_err(|_| anyhow!("curl-impersonate 请求超时：{url}"))?
        .map_err(|error| anyhow!("启动 curl-impersonate 失败（{error}）：{url}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&header_dump);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "curl-impersonate 请求失败（退出码 {}）：{}{}",
            output.status.code().unwrap_or(-1),
            if stderr.is_empty() { String::new() } else { format!("：{stderr}") },
            url
        );
    }
    let (status, body) = parse_status_body(&output.stdout)?;
    let response_headers = parse_dump_headers(&header_dump);
    let _ = std::fs::remove_file(&header_dump);
    Ok((status, body, response_headers))
}

/// 使用 curl-impersonate（Chrome TLS 指纹）下载 TikTok 媒体文件到本地。
/// TikTok 的 Akamai CDN 会按 TLS/JA3 与 HTTP/2 指纹拒绝 reqwest/OpenSSL
/// 客户端（对 playAddr 返回 HTTP 403 Access Denied），必须走 curl-impersonate
/// 才能取到视频直链。支持断点续传（-C -），失败时抛错由调用方切换备用直链。
pub(crate) async fn tiktok_impersonated_download(
    url: &str,
    output_path: &Path,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<()> {
    let binary = ensure_tiktok_impersonate().await?;
    let mut command = Command::new(&binary);
    command
        .arg("--impersonate")
        .arg("chrome131")
        .arg("-s")
        .arg("-L")
        .arg("--compressed")
        .arg("-f")
        .arg("-C")
        .arg("-")
        .arg("--connect-timeout")
        .arg("30")
        .arg("--max-time")
        .arg(timeout.as_secs().to_string())
        .arg("--output")
        .arg(output_path)
        .kill_on_drop(true);

    let package = current_package();
    let mut cacert_used = false;
    if let Some(package) = &package {
        let managed_cacert = cacert_path_for(package);
        if managed_cacert.is_file() {
            command.arg("--cacert").arg(&managed_cacert);
            cacert_used = true;
        }
    }
    if !cacert_used {
        if let Ok(configured) = std::env::var("BILI_SYNC_CURL_IMPERSONATE_CACERT") {
            let configured = PathBuf::from(configured);
            if configured.is_file() {
                command.arg("--cacert").arg(&configured);
                cacert_used = true;
            }
        }
    }
    if !cacert_used {
        #[cfg(target_os = "linux")]
        if Path::new("/etc/ssl/certs/ca-certificates.crt").is_file() {
            command.arg("--cacert").arg("/etc/ssl/certs/ca-certificates.crt");
            cacert_used = true;
        }
        if !cacert_used {
            warn!("未找到 curl-impersonate 可用的 CA 证书，TikTok 媒体下载可能因 TLS 校验失败");
        }
    }

    let proxy = configured_external_proxy();
    if !proxy.is_empty() {
        command.arg("--proxy").arg(&proxy);
    }
    for (name, value) in headers {
        if let Ok(header_line) = format_header(name, value) {
            command.arg("-H").arg(header_line);
        }
    }
    command.arg(url);

    let cmd_output = tokio::time::timeout(timeout + Duration::from_secs(30), command.output())
        .await
        .map_err(|_| anyhow!("curl-impersonate 媒体下载超时：{url}"))?
        .map_err(|error| anyhow!("启动 curl-impersonate 媒体下载失败（{error}）：{url}"))?;
    if !cmd_output.status.success() {
        let stderr = String::from_utf8_lossy(&cmd_output.stderr).trim().to_string();
        bail!(
            "curl-impersonate 媒体下载失败（退出码 {}）{}：{url}",
            cmd_output.status.code().unwrap_or(-1),
            if stderr.is_empty() { String::new() } else { format!("：{stderr}") }
        );
    }
    Ok(())
}

/// 解析 curl --dump-header 输出中的响应头，返回 (小写名称, 值) 列表。
/// 跳过状态行（HTTP/...）与空行；代理下的 CONNECT 响应头也会被忽略。
fn parse_dump_headers(path: &Path) -> Vec<(String, String)> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut headers = Vec::new();
    for line in content.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with("HTTP/") {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if !name.is_empty() && !value.is_empty() {
            headers.push((name, value));
        }
    }
    headers
}

/// 校验并格式化 HTTP 请求头，拒绝包含换行的非法头。
fn format_header(name: &str, value: &str) -> Result<String> {
    if name.is_empty() || value.is_empty() {
        bail!("curl-impersonate 请求头为空：{name}");
    }
    if name.contains('\n') || name.contains('\r') || value.contains('\n') || value.contains('\r') {
        bail!("curl-impersonate 请求头包含非法换行：{name}");
    }
    Ok(format!("{name}: {value}"))
}

fn parse_status_body(stdout: &[u8]) -> Result<(u16, Vec<u8>)> {
    let marker = STATUS_MARKER.as_bytes();
    if let Some(pos) = find_last(stdout, marker) {
        let status_text = String::from_utf8_lossy(&stdout[pos + marker.len()..]).trim().to_string();
        let status: u16 = status_text
            .parse()
            .with_context(|| format!("解析 curl-impersonate HTTP 状态码失败：{status_text}"))?;
        let mut body = stdout[..pos].to_vec();
        // curl 用 `\r\n` 分隔 write-out 输出。
        if body.ends_with(b"\r\n") {
            body.truncate(body.len() - 2);
        }
        return Ok((status, body));
    }
    bail!("curl-impersonate 输出缺少 HTTP 状态码标记")
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut pos = None;
    let mut start = 0;
    while let Some(found) = haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        pos = Some(start + found);
        start = pos.unwrap() + needle.len();
    }
    pos
}
