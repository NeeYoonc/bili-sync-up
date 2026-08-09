//! TikTok 浏览器会话模拟（Rust CDP 控制系统 Chrome）。
//!
//! ## 背景
//! TikTok 的登录会话绑定浏览器 localStorage 状态（webmssdk 生成/存储的
//! msToken、web_runtime_security_uid、security-sdk 密钥、__tea_cache_tokens 等）
//! 与出口链路。仅导入 cookies.txt 会被判定为非登录环境（botType=others）。
//! 实测：curl_cffi 各种 Chrome 指纹模拟、Playwright 全新 profile 均失败；
//! 复制用户 localStorage（85 键）+ 真实 cookie + headed Chrome 后，
//! common-app-context / favorite(item_list) / followings(user/list) 全部成功。
//! 注意：旧版 headless（--headless）下 user/list 返回 403；新版 headless
//! （--headless=new，Chrome 109+）指纹与有头模式一致，实测 user/list 200。
//!
//! ## 方案
//! 浏览器扩展导出 cookies + localStorage 到 bili-sync；服务端用系统 Chrome：
//!   1. 临时 profile + CDP 启动 Chrome（headed，窗口移出屏幕）；
//!   2. 注入 tiktok-cookies.txt + tiktok-localstorage.json；
//!   3. 打开 TikTok 页面，webmssdk 用注入状态初始化；
//!   4. 导航作者主页，捕获页面自动发起的 /api/user/list/（关注列表，自带签名）；
//!   5. 页面内 fetch /api/favorite/item_list/（我的喜欢）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::config::CONFIG_DIR;
use crate::youtube::{YouTubeSearchResult};

/// 整体操作超时（含 Chrome 启动、页面加载、webmssdk 初始化）。
const BROWSER_OP_TIMEOUT: Duration = Duration::from_secs(150);
/// 等待页面自动发起 user/list 的时间（覆盖 /following 与作者主页两个页面）。
const USER_LIST_WAIT: Duration = Duration::from_secs(60);

/// localStorage 导出文件路径（扩展同步写入）。
pub fn tiktok_localstorage_path() -> PathBuf {
    CONFIG_DIR.join("tiktok-localstorage.json")
}

pub fn has_tiktok_browser_session() -> bool {
    tiktok_localstorage_path().is_file()
}

/// 浏览器模拟抓取的结果。
#[derive(Clone)]
pub struct TikTokBrowserResult {
    /// 我的喜欢（favorite）视频列表，元素为 TikTok API item。
    pub favorite_items: Vec<Value>,
    /// 关注列表（user/list），元素为 user 对象。
    pub following_users: Vec<Value>,
}

/// 浏览器抓取结果缓存：两个接口（我的喜欢/关注列表）共享一次 Chrome 会话。
static BROWSER_CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<(std::time::Instant, TikTokBrowserResult)>>> =
    std::sync::OnceLock::new();

const BROWSER_CACHE_TTL: Duration = Duration::from_secs(60);

/// 读取 localStorage 导出文件。
fn read_localstorage() -> Result<HashMap<String, String>> {
    let path = tiktok_localstorage_path();
    let contents = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "未找到 TikTok 浏览器会话文件 {}，请在浏览器扩展中点击“同步 TikTok 会话”后重试",
            path.display()
        )
    })?;
    let map: HashMap<String, Value> =
        serde_json::from_str(&contents).context("解析 tiktok-localstorage.json 失败")?;
    let mut out = HashMap::new();
    for (key, value) in map {
        if let Some(text) = value.as_str() {
            out.insert(key, text.to_string());
        } else {
            out.insert(key, value.to_string());
        }
    }
    Ok(out)
}

/// 探测系统 Chrome 可执行文件路径。
fn find_chrome() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("BILI_SYNC_CHROME_PATH") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates = if cfg!(windows) {
        vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"$LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
            r"$PROGRAMFILES\Google\Chrome\Application\chrome.exe",
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ]
    } else {
        vec![
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ]
    };
    for raw in candidates {
        let path = if raw.contains("$LOCALAPPDATA") {
            std::env::var("LOCALAPPDATA")
                .map(|base| PathBuf::from(base).join(raw.trim_start_matches("$LOCALAPPDATA\\")))
                .unwrap_or_default()
        } else if raw.contains("$PROGRAMFILES") {
            std::env::var("PROGRAMFILES")
                .map(|base| PathBuf::from(base).join(raw.trim_start_matches("$PROGRAMFILES\\")))
                .unwrap_or_default()
        } else {
            PathBuf::from(raw)
        };
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// 选取空闲端口。
fn pick_free_port() -> u16 {
    use std::net::TcpListener;
    for _ in 0..20 {
        if let Ok(listener) = TcpListener::bind("127.0.0.1:0") {
            if let Ok(port) = listener.local_addr() {
                return port.port();
            }
        }
    }
    9333
}

/// 临时 profile 目录。
fn temp_profile_dir() -> PathBuf {
    let nonce: String = (0..8)
        .map(|_| {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u32)
                .unwrap_or(0);
            char::from(b'a' + (n % 26) as u8)
        })
        .collect();
    std::env::temp_dir().join(format!("bili-sync-tiktok-{}-{nonce}", std::process::id()))
}

/// 启动 Chrome 并等待 DevTools 端口就绪，返回 (进程句柄, ws 地址, profile 目录)。
async fn launch_chrome() -> Result<(Child, String, PathBuf)> {
    let chrome = find_chrome()
        .ok_or_else(|| anyhow!("未找到 Chrome，请安装 Chrome 后重试（TikTok 浏览器会话需要真实 Chrome）"))?;
    let port = pick_free_port();
    let profile = temp_profile_dir();
    info!(chrome = %chrome.display(), port, "TikTok 浏览器模拟：启动 Chrome");
    std::fs::create_dir_all(&profile).context("创建 Chrome 临时 profile 失败")?;
    let mut command = Command::new(&chrome);
    command
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--remote-allow-origins=*")
        // 新版 headless（--headless=new，Chrome 109+）指纹与有头模式一致，实测 user/list 200
        .arg("--headless=new")
        .arg("--hide-scrollbars")
        .arg("--mute-audio")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("about:blank")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(not(windows))]
    {
        // Linux/容器无沙箱环境必须禁用 setuid sandbox 与共享内存
        command.arg("--disable-dev-shm-usage");
    }
    let child = command.spawn().with_context(|| format!("启动 Chrome 失败：{chrome:?}"))?;

    let client = reqwest::Client::new();
    // 等待 DevTools 端口就绪
    for _ in 0..60 {
        let url = format!("http://127.0.0.1:{port}/json/list");
        if let Ok(response) = client.get(&url).send().await {
            if response.status().is_success() {
                if let Ok(pages) = response.json::<Vec<Value>>().await {
                    for page in pages {
                        if page.get("type").and_then(Value::as_str) == Some("page") {
                            if let Some(ws) = page.get("webSocketDebuggerUrl").and_then(Value::as_str) {
                                info!(ws_url = %ws, "TikTok 浏览器模拟：DevTools 已就绪");
                                return Ok((child, ws.to_string(), profile));
                            }
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // 失败清理
    let _ = std::fs::remove_dir_all(&profile);
    Err(anyhow!("Chrome DevTools 端口未就绪"))
}

/// 终止 Chrome 进程（Windows 用 taskkill 递归，其他平台直接 kill）。
fn kill_chrome(mut child: Child) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// CDP 客户端句柄。
#[derive(Clone)]
struct Cdp {
    tx: mpsc::Sender<String>,
    pending: Arc<DashMap<u64, oneshot::Sender<Value>>>,
    next_id: Arc<AtomicU64>,
    events: broadcast::Sender<Value>,
}

impl Cdp {
    async fn connect(ws_url: &str) -> Result<(Self, broadcast::Receiver<Value>)> {
        info!("TikTok 浏览器模拟：连接 CDP");
        let (ws, _) = connect_async(ws_url)
            .await
            .with_context(|| format!("连接 Chrome DevTools 失败：{ws_url}"))?;
        let (mut ws_tx, mut ws_rx) = ws.split();
        let (tx, mut rx) = mpsc::channel::<String>(256);
        let pending: Arc<DashMap<u64, oneshot::Sender<Value>>> = Arc::new(DashMap::new());
        let (events, event_rx) = broadcast::channel(512);
        let events_sender = events.clone();
        let pending_reader = pending.clone();

        // 出站 writer
        tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if ws_tx.send(WsMessage::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        // 入站 reader：响应分发 + 事件广播
        tokio::spawn(async move {
            while let Some(Ok(message)) = ws_rx.next().await {
                if let WsMessage::Text(text) = message {
                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                        if let Some(id) = value.get("id").and_then(Value::as_u64) {
                            if let Some((_, responder)) = pending_reader.remove(&id) {
                                let _ = responder.send(value);
                            }
                        } else if value.get("method").is_some() {
                            let _ = events_sender.send(value);
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                tx,
                pending,
                next_id: Arc::new(AtomicU64::new(1)),
                events: events.clone(),
            },
            event_rx,
        ))
    }

    async fn cmd(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (responder, rx) = oneshot::channel();
        self.pending.insert(id, responder);
        let message = json!({ "id": id, "method": method, "params": params });
        self.tx
            .send(message.to_string())
            .await
            .context("发送 CDP 命令失败")?;
        match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(response)) => {
                if let Some(error) = response.get("error") {
                    bail!("CDP {method} 错误：{error}");
                }
                Ok(response.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(_)) => bail!("CDP {method} 响应通道关闭"),
            Err(_) => bail!("CDP {method} 超时"),
        }
    }

    /// 在页面主世界执行表达式并返回值。
    async fn evaluate(&self, expression: &str) -> Result<Value> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            bail!("页面脚本异常：{exception}");
        }
        // Runtime.evaluate 返回 { type, value, ... }，实际值在 value 字段
        Ok(result
            .get("result")
            .and_then(|v| v.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn navigate(&self, url: &str) -> Result<()> {
        self.cmd("Page.navigate", json!({ "url": url })).await?;
        Ok(())
    }

    /// 等待页面加载完成。
    async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last_state = String::new();
        loop {
            let ready = self
                .evaluate("document.readyState")
                .await
                .unwrap_or(Value::Null);
            let state = ready.as_str().unwrap_or("?");
            if state != last_state {
                info!(state, "TikTok 浏览器模拟：页面加载状态");
                last_state = state.to_string();
            }
            if state == "complete" {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("等待 TikTok 页面加载超时（最后状态 {state}）");
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    }

    /// 通过 CDP 注入 tiktok-cookies.txt 中的 Cookie（登录必需）。
    async fn inject_cookies(&self) -> Result<usize> {
        let path = crate::tiktok::tiktok_cookie_path();
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 TikTok cookies.txt 失败：{}", path.display()))?;
        let mut cookies = Vec::new();
        for line in contents.lines() {
            let http_only = line.starts_with("#HttpOnly_");
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.trim_start().starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let columns: Vec<&str> = line.split('\t').collect();
            if columns.len() < 7 {
                continue;
            }
            let domain = columns[0].trim();
            if !domain.contains("tiktok") && !domain.contains("tiktokcdn") {
                continue;
            }
            let expires = columns[4].parse::<f64>().unwrap_or(0.0);
            let mut cookie = serde_json::Map::new();
            cookie.insert("name".into(), json!(columns[5]));
            cookie.insert("value".into(), json!(columns[6]));
            cookie.insert("domain".into(), json!(domain));
            cookie.insert("path".into(), json!(columns[2]));
            cookie.insert("secure".into(), json!(columns[3].eq_ignore_ascii_case("TRUE")));
            cookie.insert("httpOnly".into(), json!(http_only));
            if expires > 0.0 {
                cookie.insert("expires".into(), json!(expires as i64));
            }
            cookies.push(Value::Object(cookie));
        }
        if cookies.is_empty() {
            bail!("TikTok cookies.txt 中未找到 tiktok.com Cookie，请先导入登录状态");
        }
        self.cmd("Network.setCookies", json!({ "cookies": cookies })).await?;
        Ok(cookies.len())
    }

    /// 注入 localStorage（在 tiktok.com 页面上下文执行）。
    async fn inject_localstorage(&self, entries: &HashMap<String, String>) -> Result<usize> {
        let items = entries
            .iter()
            .map(|(k, v)| (k.replace('\\', "\\\\").replace('\'', "\\'"), v.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>();
        // 分块注入，避免单次表达式过大
        let mut injected = 0usize;
        for chunk in items.chunks(20) {
            let pairs = chunk
                .iter()
                .map(|(k, v)| format!("localStorage.setItem('{k}', {v:?});"))
                .collect::<Vec<_>>()
                .join(" ");
            let expr = format!("(() => {{ {pairs} return {n}; }})()", n = chunk.len());
            let result = self.evaluate(&expr).await?;
            injected += result.as_i64().unwrap_or(0) as usize;
        }
        Ok(injected)
    }

    /// 页面内 fetch 并返回 JSON（处理 base64 响应）。
    async fn page_fetch_json(&self, path_and_query: &str) -> Result<Value> {
        let path = path_and_query.replace('\\', "\\\\").replace('\'', "\\'");
        let expr = format!(
            "(async () => {{ const r = await fetch('{path}', {{ credentials: 'include' }}); const t = await r.text(); try {{ return JSON.parse(t); }} catch {{ try {{ return JSON.parse(atob(t)); }} catch {{ return {{ __raw: t.slice(0, 500) }}; }} }} }})()"
        );
        self.evaluate(&expr).await
    }
}

/// 主入口：浏览器模拟抓取我的喜欢 + 关注列表。
pub async fn fetch_tiktok_browser_data(limit: usize) -> Result<TikTokBrowserResult> {
    // 缓存命中（60 秒内）直接复用，避免我的喜欢/关注列表各启动一次 Chrome
    let cache = BROWSER_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    if let Some((created_at, cached)) = cache.lock().await.clone() {
        if created_at.elapsed() < BROWSER_CACHE_TTL {
            return Ok(cached);
        }
    }

    let localstorage = read_localstorage()?;

    let result = tokio::time::timeout(BROWSER_OP_TIMEOUT, async {
        let (child, ws_url, profile) = launch_chrome().await?;
        let _guard = ChromeGuard { child: Some(child), profile: Some(profile.clone()) };
        let (cdp, mut events) = Cdp::connect(&ws_url).await?;
        cdp.cmd("Runtime.enable", json!({})).await?;
        cdp.cmd("Page.enable", json!({})).await?;
        cdp.cmd("Network.enable", json!({})).await?;
        // 等待初始执行上下文建立
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // 0. 注入登录 Cookie（与 localStorage 一起构成完整浏览器会话）
        let cookie_count = cdp.inject_cookies().await?;
        info!(cookie_count, "TikTok 浏览器会话已注入 Cookie");

        // 1. 打开 TikTok（空 localStorage 页面）
        cdp.navigate("https://www.tiktok.com/").await?;
        cdp.wait_ready(Duration::from_secs(45)).await?;
        // 等 webmssdk 初始化后再注入，避免被覆盖
        tokio::time::sleep(Duration::from_secs(3)).await;

        // 2. 注入 localStorage
        let injected = cdp.inject_localstorage(&localstorage).await?;
        info!(injected, "TikTok 浏览器会话已注入 localStorage");

        // 3. 重新加载让 webmssdk 用注入状态初始化，并获取账号信息
        cdp.navigate("https://www.tiktok.com/@explore").await?;
        cdp.wait_ready(Duration::from_secs(45)).await?;
        // webmssdk 初始化（刷新 msToken/签名器）需要时间，多等并重试
        tokio::time::sleep(Duration::from_secs(12)).await;

        let mut sec_uid = String::new();
        let mut unique_id = String::new();
        for attempt in 0..4 {
            let ctx = cdp
                .page_fetch_json("/node-webapp/api/common-app-context?lang=zh-Hans")
                .await?;
            sec_uid = ctx
                .pointer("/user/secUid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            unique_id = ctx
                .pointer("/user/uniqueId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !sec_uid.is_empty() {
                break;
            }
            warn!(attempt, "TikTok 浏览器会话验证暂未通过，等待后重试");
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
        if sec_uid.is_empty() {
            bail!("TikTok 浏览器会话未通过验证：请重新同步会话（登录 TikTok 后刷新页面再导出）");
        }
        info!(unique_id, "TikTok 浏览器会话验证通过");

        // 4. 捕获关注列表（页面自动发起带签名的 /api/user/list/ 请求）。
        //    先启动持续监听再导航，避免错过页面发出的请求；
        //    /following 页面与作者主页在不同条件下都会触发，先试前者，失败再回退后者。
        let mut followings = Vec::new();
        if !unique_id.is_empty() {
            let watcher = UserListWatcher::start(cdp.clone(), events, USER_LIST_WAIT).await;
            let pages = vec![
                "https://www.tiktok.com/following".to_string(),
                format!("https://www.tiktok.com/@{unique_id}"),
            ];
            for url in pages {
                if !followings.is_empty() {
                    break;
                }
                cdp.navigate(&url).await?;
                // 轮询监听结果；滚动触发懒加载
                for _ in 0..3 {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    if let Some(list) = watcher.take().await {
                        followings = list;
                        break;
                    }
                    let _ = cdp.evaluate("window.scrollBy(0, 800); true").await;
                }
            }
            watcher.abort().await;
        }

        // 5. 页面内 fetch favorite（我的喜欢，webmssdk 自动签名）
        let favorite_items = fetch_favorite_paged(&cdp, &sec_uid, limit).await?;

        let fetched = TikTokBrowserResult {
            favorite_items,
            following_users: followings,
        };
        *cache.lock().await = Some((std::time::Instant::now(), fetched.clone()));
        Ok(fetched)
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => Err(anyhow!("TikTok 浏览器模拟超时（{} 秒）", BROWSER_OP_TIMEOUT.as_secs())),
    }
}

/// Chrome 进程与临时目录守卫（作用域退出时清理）。
struct ChromeGuard {
    child: Option<Child>,
    profile: Option<PathBuf>,
}

impl Drop for ChromeGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            kill_chrome(child);
        }
        if let Some(profile) = self.profile.take() {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

/// 关注列表（/api/user/list/）响应监听器：独立任务持续捕获页面自动发起的
/// user/list 请求响应，主流程导航后可轮询取结果，避免错过请求时序。
struct UserListWatcher {
    result: Arc<tokio::sync::Mutex<Option<Vec<Value>>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl UserListWatcher {
    async fn start(cdp: Cdp, mut events: broadcast::Receiver<Value>, timeout: Duration) -> Self {
        let result: Arc<tokio::sync::Mutex<Option<Vec<Value>>>> = Default::default();
        let result_ref = result.clone();
        let handle = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + timeout;
            let mut seen_request: Option<String> = None;
            while tokio::time::Instant::now() < deadline {
                let remaining = deadline - tokio::time::Instant::now();
                let event = match tokio::time::timeout(remaining, events.recv()).await {
                    Ok(Ok(event)) => event,
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
                };
                let method = event.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "Network.responseReceived" {
                    let url = event["params"]["response"]["url"].as_str().unwrap_or("");
                    if url.contains("/api/user/list/") {
                        seen_request = event["params"]["requestId"].as_str().map(str::to_string);
                    }
                } else if method == "Network.loadingFinished" {
                    let request_id = event["params"]["requestId"]
                        .as_str()
                        .map(str::to_string);
                    if request_id.is_some() && request_id == seen_request {
                        let request_id = request_id.unwrap();
                        if let Ok(result) = cdp
                            .cmd("Network.getResponseBody", json!({ "requestId": request_id }))
                            .await
                        {
                            let body = result.get("body").and_then(Value::as_str).unwrap_or("");
                            let base64_encoded = result
                                .get("base64Encoded")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let raw = if base64_encoded {
                                match base64::engine::general_purpose::STANDARD.decode(body) {
                                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                                    Err(_) => continue,
                                }
                            } else {
                                body.to_string()
                            };
                            if let Ok(payload) = serde_json::from_str::<Value>(&raw) {
                                let list = payload
                                    .get("userList")
                                    .and_then(Value::as_array)
                                    .cloned()
                                    .unwrap_or_default();
                                info!(count = list.len(), "TikTok 浏览器捕获关注列表");
                                *result_ref.lock().await = Some(list);
                                break;
                            }
                        }
                    }
                }
            }
        });
        Self { result, handle }
    }

    /// 取已捕获的关注列表（无结果返回 None）。
    async fn take(&self) -> Option<Vec<Value>> {
        self.result.lock().await.clone()
    }

    /// 停止监听并等待任务结束。
    async fn abort(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

/// 分页抓取我的喜欢。
async fn fetch_favorite_paged(cdp: &Cdp, sec_uid: &str, limit: usize) -> Result<Vec<Value>> {
    let mut cursor = 0i64;
    let mut items = Vec::new();
    for _ in 0..50 {
        let params = format!(
            "aid=1988&app_language=zh-Hans&app_name=tiktok_web&browser_language=zh-CN&browser_name=Mozilla&browser_online=true&browser_platform=Win32&browser_version={ua}&channel=tiktok_web&cookie_enabled=true&count=30&cursor={cursor}&data_collection_enabled=true&device_platform=web_pc&focus_state=true&from_page=user&history_len=7&is_fullscreen=false&is_page_visible=true&language=zh-Hans&needPinnedItemIds=true&os=windows&priority_region=US&referer=https%3A%2F%2Fwww.tiktok.com%2F&region=US&root_referer=https%3A%2F%2Fwww.tiktok.com%2F&screen_height=720&screen_width=1280&secUid={sec_uid}&tz_name=Asia%2FShanghai&user_is_login=true&video_encoding=dash&webcast_language=zh-Hans",
            ua = "Mozilla%2F5.0%20(Windows%20NT%2010.0%3B%20Win64%3B%20x64)%20AppleWebKit%2F537.36%20(KHTML%2C%20like%20Gecko)%20Chrome%2F150.0.0.0%20Safari%2F537.36",
        );
        let payload = cdp
            .page_fetch_json(&format!("/api/favorite/item_list/?{params}"))
            .await?;
        if let Some(list) = payload.get("itemList").and_then(Value::as_array) {
            let mut page_added = 0usize;
            for item in list {
                if items.len() >= limit {
                    break;
                }
                items.push(item.clone());
                page_added += 1;
            }
            if items.len() >= limit || page_added == 0 {
                break;
            }
        } else {
            break;
        }
        let next = payload
            .get("cursor")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<i64>().ok())
            .or_else(|| payload.get("cursor").and_then(Value::as_i64))
            .unwrap_or(cursor);
        if next <= cursor {
            break;
        }
        cursor = next;
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    Ok(items)
}

/// 将 favorite item 转成 crate 的 TikTokPost。
pub fn browser_items_to_posts(items: &[Value]) -> Vec<crate::tiktok::TikTokPost> {
    items
        .iter()
        .filter_map(crate::tiktok::parse_tiktok_item)
        .collect()
}

/// 将 user/list 条目转成 YouTubeSearchResult。
pub fn browser_users_to_results(users: &[Value]) -> Vec<YouTubeSearchResult> {
    users
        .iter()
        .map(|entry| entry.get("user").unwrap_or(entry))
        .filter_map(crate::tiktok::tiktok_user_to_search_result)
        .collect()
}
