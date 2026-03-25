//! DeepSeek Web API POW (Proof of Work) 求解器
//!
//! DeepSeek 使用自建的 WASM 算法，非标准 Keccak-256
//! 本模块通过 wasmtime 直接加载 WASM 求解 POW
//!
//! WASM 自动更新：从 DeepSeek 网站获取最新 WASM 并缓存到配置目录

use once_cell::sync::Lazy;
use regex::Regex;
use sha3::{Digest, Keccak256};
use std::path::PathBuf;
use tracing::{debug, info, warn};
#[cfg(not(target_arch = "arm"))]
use wasmtime::*;

use crate::config::CONFIG_DIR;

/// WASM 哈希文件路径
fn wasm_hash_file() -> PathBuf {
    CONFIG_DIR.join(".wasm_hash")
}

/// WASM 文件路径
fn wasm_file_path() -> PathBuf {
    CONFIG_DIR.join("sha3_wasm.wasm")
}

/// 全局 WASM 更新状态（记录上次成功检查的时间）
static WASM_LAST_CHECK: Lazy<parking_lot::Mutex<Option<std::time::Instant>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));

/// 从 DeepSeek 网站获取最新 WASM 哈希
async fn fetch_latest_wasm_hash() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;

    // 1. 获取主页 HTML
    let html = client
        .get("https://chat.deepseek.com/")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    // 提取 main.js URL
    let main_js_re = Regex::new(r#"src="(https://static\.deepseek\.com/chat/static/main\.[^"]+\.js)""#).ok()?;
    let main_js_url = main_js_re.captures(&html)?.get(1)?.as_str();

    debug!("获取 main.js: {}", main_js_url);

    // 2. 获取 main.js 内容
    let main_js = client
        .get(main_js_url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    // 尝试从 main.js 提取 WASM 哈希
    let wasm_re = Regex::new(r"sha3_wasm_bg\.([a-f0-9]+)\.wasm").ok()?;
    if let Some(caps) = wasm_re.captures(&main_js) {
        let hash = caps.get(1)?.as_str().to_string();
        debug!("从 main.js 提取到 WASM 哈希: {}", hash);
        return Some(hash);
    }

    // 3. 搜索 chunk 文件
    let chunk_re = Regex::new(r"\d{4,5}\.[a-f0-9]+\.js").ok()?;
    let chunks: Vec<_> = chunk_re
        .find_iter(&main_js)
        .map(|m| m.as_str().to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .take(10)
        .collect();

    for chunk in chunks {
        let chunk_url = format!("https://static.deepseek.com/chat/static/{}", chunk);
        if let Ok(resp) = client.get(&chunk_url).header("User-Agent", "Mozilla/5.0").send().await {
            if let Ok(chunk_js) = resp.text().await {
                if let Some(caps) = wasm_re.captures(&chunk_js) {
                    let hash = caps.get(1)?.as_str().to_string();
                    debug!("从 chunk {} 提取到 WASM 哈希: {}", chunk, hash);
                    return Some(hash);
                }
            }
        }
    }

    None
}

/// 下载 WASM 文件
async fn download_wasm(hash: &str) -> anyhow::Result<Vec<u8>> {
    let url = format!("https://static.deepseek.com/chat/static/sha3_wasm_bg.{}.wasm", hash);

    debug!("下载 WASM: {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let bytes = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await?
        .bytes()
        .await?;

    // 验证 WASM 魔数
    if bytes.len() < 4 || bytes[0] != 0x00 || bytes[1] != 0x61 || bytes[2] != 0x73 || bytes[3] != 0x6d {
        anyhow::bail!("下载的文件不是有效的 WASM");
    }

    info!("已下载 WASM ({:.1} KB)", bytes.len() as f64 / 1024.0);
    Ok(bytes.to_vec())
}

/// 检查 WASM 文件是否存在且有效
fn is_wasm_valid() -> bool {
    if let Ok(bytes) = std::fs::read(wasm_file_path()) {
        bytes.len() >= 4 && bytes[0] == 0x00 && bytes[1] == 0x61 && bytes[2] == 0x73 && bytes[3] == 0x6d
    } else {
        false
    }
}

/// 检查并更新 WASM 文件
/// 每次调用都会检查本地文件是否存在，但网络更新检查每小时最多一次
pub async fn check_and_update_wasm() -> bool {
    // 如果本地文件不存在或无效，强制下载
    if !is_wasm_valid() {
        info!("WASM 文件不存在或无效，正在下载...");
        match do_check_and_update_wasm().await {
            Ok(updated) => {
                if updated {
                    // 更新检查时间
                    *WASM_LAST_CHECK.lock() = Some(std::time::Instant::now());
                }
                return updated;
            }
            Err(e) => {
                warn!("WASM 下载失败: {}", e);
                return false;
            }
        }
    }

    // 本地文件有效，检查是否需要更新（每小时最多检查一次）
    let should_check = {
        let guard = WASM_LAST_CHECK.lock();
        guard
            .map(|t| t.elapsed() > std::time::Duration::from_secs(3600))
            .unwrap_or(true)
    };

    if should_check {
        match do_check_and_update_wasm().await {
            Ok(updated) => {
                *WASM_LAST_CHECK.lock() = Some(std::time::Instant::now());
                updated
            }
            Err(e) => {
                debug!("WASM 更新检查失败: {}", e);
                // 更新检查时间，避免频繁重试
                *WASM_LAST_CHECK.lock() = Some(std::time::Instant::now());
                false
            }
        }
    } else {
        false
    }
}

/// 执行 WASM 更新检查
async fn do_check_and_update_wasm() -> anyhow::Result<bool> {
    // 获取最新哈希
    let latest_hash = match fetch_latest_wasm_hash().await {
        Some(h) => h,
        None => {
            debug!("无法获取最新 WASM 信息，使用本地文件");
            return Ok(false);
        }
    };

    // 读取本地哈希
    let local_hash = std::fs::read_to_string(wasm_hash_file()).unwrap_or_default();
    let local_hash = local_hash.trim();

    // 检查本地文件是否存在且有效
    let local_valid = if wasm_file_path().exists() {
        if let Ok(bytes) = std::fs::read(wasm_file_path()) {
            bytes.len() >= 4 && bytes[0] == 0x00 && bytes[1] == 0x61 && bytes[2] == 0x73 && bytes[3] == 0x6d
        } else {
            false
        }
    } else {
        false
    };

    // 如果哈希相同且文件有效，无需更新
    if local_hash == latest_hash && local_valid {
        debug!("WASM 已是最新版本: {}", latest_hash);
        return Ok(false);
    }

    // 需要更新
    if !local_valid {
        info!("首次下载 WASM 或本地文件损坏，正在下载...");
    } else if local_hash != latest_hash {
        info!("发现新版本 WASM: {} -> {}", local_hash, latest_hash);
    }

    // 下载新版本
    let wasm_bytes = download_wasm(&latest_hash).await?;

    // 确保目录存在
    if let Some(parent) = wasm_file_path().parent() {
        std::fs::create_dir_all(parent)?;
    }

    // 保存 WASM 文件
    std::fs::write(wasm_file_path(), &wasm_bytes)?;

    // 保存哈希
    std::fs::write(wasm_hash_file(), &latest_hash)?;

    info!("WASM 已更新到: {}", latest_hash);

    // 重新初始化 WASM 运行时
    reinit_wasm_runtime();

    Ok(true)
}

/// POW 挑战响应数据
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PowChallenge {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub difficulty: u64,
    pub expire_at: i64,
    pub signature: String,
    pub target_path: String,
}

/// POW 求解结果（用于发送给服务器）
#[derive(Debug, Clone, serde::Serialize)]
pub struct PowResponse {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub answer: u64,
    pub signature: String,
    pub target_path: String,
}

/// WASM 运行时（全局单例，支持重新初始化）
#[cfg(not(target_arch = "arm"))]
static WASM_RUNTIME: Lazy<parking_lot::RwLock<Option<WasmPowSolver>>> =
    Lazy::new(|| parking_lot::RwLock::new(init_wasm_solver()));

/// 初始化 WASM 求解器
#[cfg(not(target_arch = "arm"))]
fn init_wasm_solver() -> Option<WasmPowSolver> {
    match WasmPowSolver::new() {
        Ok(solver) => {
            info!("WASM POW 求解器初始化成功");
            Some(solver)
        }
        Err(e) => {
            warn!("WASM POW 求解器初始化失败: {}", e);
            None
        }
    }
}

/// 重新初始化 WASM 运行时（WASM 更新后调用）
#[cfg(not(target_arch = "arm"))]
fn reinit_wasm_runtime() {
    let mut guard = WASM_RUNTIME.write();
    *guard = init_wasm_solver();
}

#[cfg(target_arch = "arm")]
fn reinit_wasm_runtime() {}

/// WASM POW 求解器
#[cfg(not(target_arch = "arm"))]
struct WasmPowSolver {
    store: Store<()>,
    memory: Memory,
    malloc: TypedFunc<(i32, i32), i32>,
    stack_pointer: TypedFunc<i32, i32>,
    wasm_solve: TypedFunc<(i32, i32, i32, i32, i32, f64), ()>,
}

#[cfg(not(target_arch = "arm"))]
impl WasmPowSolver {
    /// 创建新的 WASM 求解器
    fn new() -> anyhow::Result<Self> {
        // 获取 WASM 字节（优先从文件系统，其次使用嵌入的）
        let wasm_bytes = Self::load_wasm_bytes()?;

        // 创建 wasmtime 引擎和存储
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());

        // 编译并实例化模块
        let module = Module::new(&engine, &wasm_bytes)?;
        let instance = Instance::new(&mut store, &module, &[])?;

        // 获取导出的函数和内存
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow::anyhow!("找不到 memory 导出"))?;

        let malloc = instance.get_typed_func::<(i32, i32), i32>(&mut store, "__wbindgen_export_0")?;

        let stack_pointer = instance.get_typed_func::<i32, i32>(&mut store, "__wbindgen_add_to_stack_pointer")?;

        let wasm_solve = instance.get_typed_func::<(i32, i32, i32, i32, i32, f64), ()>(&mut store, "wasm_solve")?;

        Ok(Self {
            store,
            memory,
            malloc,
            stack_pointer,
            wasm_solve,
        })
    }

    /// 加载 WASM 字节
    /// 从文件系统加载（配置目录或环境变量指定路径）
    fn load_wasm_bytes() -> anyhow::Result<Vec<u8>> {
        // 按优先级查找文件: 环境变量 > 配置目录 > 可执行文件同级目录
        let paths = [
            std::env::var("DEEPSEEK_WASM_PATH").ok(),
            Some(wasm_file_path().to_string_lossy().to_string()),
            std::env::current_exe().ok().and_then(|p| {
                p.parent()
                    .map(|d| d.join("sha3_wasm.wasm").to_string_lossy().to_string())
            }),
        ];

        for path in paths.iter().flatten() {
            if let Ok(bytes) = std::fs::read(path) {
                // 验证 WASM 魔数
                if bytes.len() >= 4 && bytes[0] == 0x00 && bytes[1] == 0x61 && bytes[2] == 0x73 && bytes[3] == 0x6d {
                    debug!("从文件加载 WASM: {}", path);
                    return Ok(bytes);
                }
            }
        }

        anyhow::bail!("WASM 文件不存在或无效，请等待自动下载完成后重试")
    }

    /// 向 WASM 内存写入字符串
    fn write_string(&mut self, s: &str) -> anyhow::Result<(i32, i32)> {
        let bytes = s.as_bytes();
        let len = bytes.len() as i32;

        // 分配内存
        let ptr = self.malloc.call(&mut self.store, (len, 1))?;

        // 写入数据
        self.memory.write(&mut self.store, ptr as usize, bytes)?;

        Ok((ptr, len))
    }

    /// 求解 POW
    fn solve(&mut self, challenge: &str, prefix: &str, difficulty: f64) -> anyhow::Result<u64> {
        // 分配栈空间
        let retptr = self.stack_pointer.call(&mut self.store, -16)?;

        // 写入字符串参数
        let (challenge_ptr, challenge_len) = self.write_string(challenge)?;
        let (prefix_ptr, prefix_len) = self.write_string(prefix)?;

        // 调用求解函数
        self.wasm_solve.call(
            &mut self.store,
            (retptr, challenge_ptr, challenge_len, prefix_ptr, prefix_len, difficulty),
        )?;

        // 读取结果（f64 在 retptr + 8 位置）
        let mut result_bytes = [0u8; 8];
        self.memory
            .read(&self.store, (retptr + 8) as usize, &mut result_bytes)?;
        let answer = f64::from_le_bytes(result_bytes);

        // 恢复栈指针
        self.stack_pointer.call(&mut self.store, 16)?;

        Ok(answer as u64)
    }
}

/// 求解 POW 挑战
///
/// DeepSeek 使用自建 WASM 算法，优先使用 WASM 求解器
/// 如果 WASM 不可用，回退到 Keccak-256（可能被服务器拒绝）
///
/// # 参数
/// - `challenge`: POW 挑战数据
///
/// # 返回
/// - 满足条件的 nonce 值
pub fn solve_pow(challenge: &PowChallenge) -> u64 {
    // 优先尝试使用 WASM 求解器
    if let Some(answer) = solve_pow_wasm(challenge) {
        return answer;
    }

    // 回退到 Keccak-256（可能被服务器拒绝）
    warn!("WASM 求解器不可用，回退到 Keccak-256（可能被服务器拒绝）");
    solve_pow_keccak(challenge)
}

/// 使用 WASM 求解 POW
#[cfg(not(target_arch = "arm"))]
fn solve_pow_wasm(challenge: &PowChallenge) -> Option<u64> {
    let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);

    debug!(
        "使用 WASM 求解 POW: challenge={}..., prefix={}, difficulty={}",
        &challenge.challenge[..challenge.challenge.len().min(20)],
        prefix,
        challenge.difficulty
    );

    let start = std::time::Instant::now();

    // 获取全局 WASM 运行时
    let mut guard = WASM_RUNTIME.write();
    let solver = guard.as_mut()?;

    // 调用 WASM 求解
    match solver.solve(&challenge.challenge, &prefix, challenge.difficulty as f64) {
        Ok(answer) => {
            let elapsed = start.elapsed();
            debug!(
                "WASM POW 求解成功: nonce={}, 耗时={:.2}s",
                answer,
                elapsed.as_secs_f64()
            );
            Some(answer)
        }
        Err(e) => {
            warn!("WASM POW 求解失败: {}", e);
            None
        }
    }
}

#[cfg(target_arch = "arm")]
fn solve_pow_wasm(_challenge: &PowChallenge) -> Option<u64> {
    debug!("当前 arm 架构不支持 wasmtime，跳过 WASM POW 求解");
    None
}

/// 使用 Keccak-256 求解 POW（备用方案，可能被服务器拒绝）
fn solve_pow_keccak(challenge: &PowChallenge) -> u64 {
    let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);
    let difficulty = challenge.difficulty;
    let target_prefix: u64 = if difficulty > 0 {
        u64::MAX / difficulty
    } else {
        u64::MAX
    };

    debug!(
        "使用 Keccak-256 求解 POW: prefix={}, target_prefix={:#018x}",
        &prefix, target_prefix
    );

    let start = std::time::Instant::now();

    for nonce in 0u64..100_000_000 {
        let data = format!("{}{}", prefix, nonce);
        let hash = Keccak256::digest(data.as_bytes());
        let hash_prefix = u64::from_be_bytes(hash[0..8].try_into().unwrap());

        if hash_prefix < target_prefix {
            let elapsed = start.elapsed();
            debug!(
                "Keccak POW 求解成功: nonce={}, hash_prefix={:#018x}, 耗时={:.2}s",
                nonce,
                hash_prefix,
                elapsed.as_secs_f64()
            );
            return nonce;
        }
    }

    warn!("Keccak POW 求解失败：未找到有效 nonce");
    0
}

/// 构建 POW 响应
pub fn build_pow_response(challenge: &PowChallenge, answer: u64) -> PowResponse {
    let response = PowResponse {
        algorithm: challenge.algorithm.clone(),
        challenge: challenge.challenge.clone(),
        salt: challenge.salt.clone(),
        answer,
        signature: challenge.signature.clone(),
        target_path: challenge.target_path.clone(),
    };

    debug!(
        "构建 POW 响应: algorithm={}, challenge={}..., salt={}, answer={}, target_path={}",
        response.algorithm,
        &response.challenge[..response.challenge.len().min(20)],
        response.salt,
        response.answer,
        response.target_path
    );

    response
}

/// 将 POW 响应编码为 Base64（用于 HTTP 头）
pub fn encode_pow_header(response: &PowResponse) -> String {
    let json = serde_json::to_string(response).unwrap_or_default();
    debug!("POW 响应 JSON: {}", &json[..json.len().min(200)]);
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_solve_pow_keccak() {
        // 测试 Keccak-256 备用方案
        let challenge = PowChallenge {
            algorithm: "DeepSeekHashV1".to_string(),
            challenge: "test".to_string(),
            salt: "abc".to_string(),
            difficulty: 1000,
            expire_at: 1234567890,
            signature: "sig".to_string(),
            target_path: "/api/v0/chat/completion".to_string(),
        };

        let nonce = solve_pow_keccak(&challenge);

        let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);
        let data = format!("{}{}", prefix, nonce);
        let hash = Keccak256::digest(data.as_bytes());
        let hash_prefix = u64::from_be_bytes(hash[0..8].try_into().unwrap());
        let target_prefix = u64::MAX / challenge.difficulty;

        println!(
            "Keccak test: nonce={}, hash={:#018x}, target={:#018x}",
            nonce, hash_prefix, target_prefix
        );
        assert!(hash_prefix < target_prefix);
    }

    #[test]
    fn test_solve_pow_wasm() {
        // 测试 WASM 求解器（使用嵌入的 WASM）
        let challenge = PowChallenge {
            algorithm: "DeepSeekHashV1".to_string(),
            challenge: "test_challenge_string".to_string(),
            salt: "abc123".to_string(),
            difficulty: 144000,
            expire_at: 1767571094573,
            signature: "sig".to_string(),
            target_path: "/api/v0/chat/completion".to_string(),
        };

        match solve_pow_wasm(&challenge) {
            Some(nonce) => {
                println!("WASM test: nonce={}", nonce);
            }
            None => {
                println!("WASM test: 求解器不可用，跳过测试");
            }
        }
    }
}
