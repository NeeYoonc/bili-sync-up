//! DeepSeek Web API 客户端
//!
//! 使用 chat.deepseek.com 免费 Web API 进行 AI 聊天

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::deepseek_pow::{build_pow_response, encode_pow_header, solve_pow, PowChallenge};

const BASE_URL: &str = "https://chat.deepseek.com";
const APP_VERSION: &str = "20241129.1";

/// DeepSeek Web 会话信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekSession {
    /// 会话 ID
    pub session_id: String,
    /// 最后一条消息 ID（用于连续对话）
    pub parent_message_id: Option<String>,
}

/// API 响应包装
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    code: i32,
    data: Option<ApiData<T>>,
    msg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiData<T> {
    biz_data: T,
}

/// 创建会话响应
#[derive(Debug, Deserialize)]
struct CreateSessionResponse {
    id: String,
}

/// POW 挑战响应
#[derive(Debug, Deserialize)]
struct PowChallengeResponse {
    challenge: PowChallenge,
}

/// Token 过期错误码
const TOKEN_EXPIRED_CODES: &[i32] = &[
    40100, // Unauthorized
    40101, // Token expired
    40102, // Token invalid
    40103, // Token not found
];

/// 防止重复发送通知的标志
static TOKEN_EXPIRED_NOTIFIED: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

/// 检查是否为 Token 过期/无效错误
fn is_token_error(status: reqwest::StatusCode, code: Option<i32>, msg: Option<&str>) -> bool {
    // HTTP 401 通常表示认证失败
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return true;
    }

    // 检查错误码
    if let Some(c) = code {
        if TOKEN_EXPIRED_CODES.contains(&c) {
            return true;
        }
    }

    // 检查错误消息
    if let Some(m) = msg {
        let m_lower = m.to_lowercase();
        if m_lower.contains("token")
            || m_lower.contains("unauthorized")
            || m_lower.contains("expire")
            || m_lower.contains("invalid")
            || m_lower.contains("authentication")
        {
            return true;
        }
    }

    false
}

/// 构建 Token 过期错误消息，并异步发送通知
fn token_expired_error() -> anyhow::Error {
    // 异步发送通知（只发送一次，避免重复通知）
    if !TOKEN_EXPIRED_NOTIFIED.swap(true, Ordering::SeqCst) {
        tokio::spawn(async {
            if let Err(e) = super::notification::send_deepseek_token_expired_notification().await {
                warn!("发送 Token 过期通知失败: {}", e);
            }
        });
    }

    anyhow!(
        "DeepSeek Web Token 已过期或无效，请重新获取 Token。\n\
        获取方法：\n\
        1. 浏览器打开 https://chat.deepseek.com 并登录\n\
        2. 按 F12 打开开发者工具 → Network 标签\n\
        3. 刷新页面，找到任意请求的 Authorization 头\n\
        4. 复制 Bearer 后面的值到系统设置"
    )
}

/// 重置 Token 过期通知标志（Token 更新后调用）
pub fn reset_token_expired_flag() {
    TOKEN_EXPIRED_NOTIFIED.store(false, Ordering::SeqCst);
}

fn extract_response_snapshot_content(data: &serde_json::Value) -> Option<String> {
    let fragments = data
        .get("v")
        .and_then(|v| v.get("response"))
        .and_then(|r| r.get("fragments"))
        .and_then(|f| f.as_array())?;

    let contents = collect_fragment_content(&serde_json::Value::Array(fragments.clone()));

    (!contents.is_empty()).then_some(contents)
}

fn collect_fragment_content(value: &serde_json::Value) -> String {
    fn collect(value: &serde_json::Value, output: &mut String) {
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, output);
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(content) = map.get("content").and_then(|content| content.as_str()) {
                    output.push_str(content);
                }
                if let Some(nested) = map.get("v") {
                    collect(nested, output);
                }
            }
            _ => {}
        }
    }

    let mut contents = String::new();
    collect(value, &mut contents);
    contents
}

fn preload_response_snapshot(body: &str) -> String {
    for line in body.lines() {
        if !line.starts_with("data:") {
            continue;
        }

        let data_str = line[5..].trim();
        if !data_str.contains("\"response\"") || !data_str.contains("\"fragments\"") {
            continue;
        }

        match serde_json::from_str::<serde_json::Value>(data_str) {
            Ok(data) => {
                if let Some(contents) = extract_response_snapshot_content(&data) {
                    return contents;
                }
            }
            Err(e) => debug!("SSE response fragments 快照预读解析失败: {}", e),
        }
    }

    String::new()
}

fn log_raw_sse_stream(body: &str) {
    debug!(
        "DeepSeek SSE 原始流开始: bytes={}, lines={}",
        body.len(),
        body.lines().count()
    );
    for (index, line) in body.lines().enumerate() {
        debug!("DeepSeek SSE 原始流[{}]: {}", index + 1, line);
    }
    debug!("DeepSeek SSE 原始流结束");
}

/// DeepSeek Web API 客户端
pub struct DeepSeekWebClient {
    client: Client,
    token: String,
}

impl DeepSeekWebClient {
    /// 创建新的客户端
    pub fn new(token: &str, timeout_seconds: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds.max(10)))
            .build()?;

        Ok(Self {
            client,
            token: token.to_string(),
        })
    }

    /// 获取默认请求头
    fn get_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, "*/*".parse().unwrap());
        headers.insert(reqwest::header::CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(reqwest::header::ORIGIN, BASE_URL.parse().unwrap());
        headers.insert(reqwest::header::REFERER, format!("{}/", BASE_URL).parse().unwrap());
        headers.insert(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
                .parse()
                .unwrap(),
        );
        headers.insert("x-app-version", APP_VERSION.parse().unwrap());
        headers.insert("x-client-locale", "zh_CN".parse().unwrap());
        headers.insert("x-client-platform", "web".parse().unwrap());
        headers.insert("x-client-version", "1.6.0".parse().unwrap());
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        headers
    }

    /// 创建新会话
    pub async fn create_session(&self) -> Result<String> {
        debug!("创建 DeepSeek 会话...");

        let resp = self
            .client
            .post(format!("{}/api/v0/chat_session/create", BASE_URL))
            .headers(self.get_headers())
            .json(&serde_json::json!({}))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            // 检查是否为 Token 过期
            if is_token_error(status, None, Some(&body)) {
                return Err(token_expired_error());
            }
            return Err(anyhow!("创建会话失败: HTTP {} - {}", status, body));
        }

        let data: ApiResponse<CreateSessionResponse> = resp.json().await?;

        if data.code != 0 {
            // 检查是否为 Token 过期
            if is_token_error(status, Some(data.code), data.msg.as_deref()) {
                return Err(token_expired_error());
            }
            return Err(anyhow!(
                "创建会话失败: code={}, msg={}",
                data.code,
                data.msg.unwrap_or_default()
            ));
        }

        let session_id = data.data.ok_or_else(|| anyhow!("创建会话响应无数据"))?.biz_data.id;

        info!("DeepSeek 会话创建成功: {}", session_id);
        Ok(session_id)
    }

    /// 获取 POW 挑战
    async fn get_pow_challenge(&self) -> Result<PowChallenge> {
        debug!("获取 POW 挑战...");

        let resp = self
            .client
            .post(format!("{}/api/v0/chat/create_pow_challenge", BASE_URL))
            .headers(self.get_headers())
            .json(&serde_json::json!({
                "target_path": "/api/v0/chat/completion"
            }))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if is_token_error(status, None, Some(&body)) {
                return Err(token_expired_error());
            }
            return Err(anyhow!("获取 POW 挑战失败: HTTP {} - {}", status, body));
        }

        let data: ApiResponse<PowChallengeResponse> = resp.json().await?;

        if data.code != 0 {
            if is_token_error(status, Some(data.code), data.msg.as_deref()) {
                return Err(token_expired_error());
            }
            return Err(anyhow!(
                "获取 POW 挑战失败: code={}, msg={}",
                data.code,
                data.msg.unwrap_or_default()
            ));
        }

        let challenge = data
            .data
            .ok_or_else(|| anyhow!("POW 挑战响应无数据"))?
            .biz_data
            .challenge;

        debug!(
            "POW 挑战获取成功: algorithm={}, difficulty={}",
            challenge.algorithm, challenge.difficulty
        );

        Ok(challenge)
    }

    /// 发送聊天消息并获取响应
    ///
    /// # 参数
    /// - `session_id`: 会话 ID
    /// - `parent_message_id`: 上一条消息 ID（可选，用于连续对话）
    /// - `prompt`: 用户消息
    /// - `timeout_seconds`: 读取响应的超时时间
    ///
    /// # 返回
    /// - (响应文本, 新的 message_id)
    pub async fn send_message(
        &self,
        session_id: &str,
        parent_message_id: Option<&str>,
        prompt: &str,
        timeout_seconds: u64,
    ) -> Result<(String, Option<String>)> {
        // 1. 获取并求解 POW 挑战
        let challenge = self.get_pow_challenge().await?;
        let answer = solve_pow(&challenge);
        let pow_response = build_pow_response(&challenge, answer);
        let pow_header = encode_pow_header(&pow_response);

        // 2. 构建请求
        let client_stream_id = format!(
            "{}-{}",
            chrono::Local::now().format("%Y%m%d"),
            uuid::Uuid::new_v4().to_string().replace("-", "")[..16].to_string()
        );

        // parent_message_id 需要转换为数字类型（服务器要求 u32）
        let parent_id_num: Option<u64> = parent_message_id.and_then(|s| s.parse().ok());

        let payload = serde_json::json!({
            "chat_session_id": session_id,
            "parent_message_id": parent_id_num,
            "prompt": prompt,
            "ref_file_ids": [],
            "thinking_enabled": false,
            "search_enabled": false,
            "client_stream_id": client_stream_id
        });

        debug!("发送聊天请求: session={}", session_id);

        // 3. 发送请求
        let mut headers = self.get_headers();
        headers.insert("x-ds-pow-response", pow_header.parse().unwrap());

        let resp = self
            .client
            .post(format!("{}/api/v0/chat/completion", BASE_URL))
            .headers(headers)
            .json(&payload)
            .send()
            .await?;

        debug!("收到HTTP响应: status={}", resp.status());

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if is_token_error(status, None, Some(&body)) {
                return Err(token_expired_error());
            }
            return Err(anyhow!("聊天请求失败: HTTP {} - {}", status, body));
        }

        // 4. 解析 SSE 流响应（带超时保护）
        debug!("开始读取响应体...");
        let read_timeout = Duration::from_secs(timeout_seconds.max(30));
        let body = match tokio::time::timeout(read_timeout, resp.text()).await {
            Ok(Ok(body)) => {
                debug!("响应体读取完成: {} 字节", body.len());
                body
            }
            Ok(Err(e)) => {
                return Err(anyhow!("读取响应体失败: {}", e));
            }
            Err(_) => {
                return Err(anyhow!("读取响应体超时 ({}秒)", timeout_seconds));
            }
        };

        log_raw_sse_stream(&body);
        let (response_text, message_id) = self.parse_sse_response(&body)?;

        Ok((response_text, message_id))
    }

    /// 解析 SSE 流响应
    fn parse_sse_response(&self, body: &str) -> Result<(String, Option<String>)> {
        // 首先检查是否是 JSON 错误响应（非 SSE 格式）
        // POW 验证失败等情况下，服务器返回 {"code":40301,"msg":"Invalid PoW response","data":null}
        if body.starts_with('{') && !body.contains("data:") {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                if let Some(code) = json.get("code").and_then(|c| c.as_i64()) {
                    if code != 0 {
                        let msg = json.get("msg").and_then(|m| m.as_str()).unwrap_or("未知错误");
                        // 检查是否为 Token 过期
                        if is_token_error(
                            reqwest::StatusCode::OK, // 已经是 200 了，只检查 code 和 msg
                            Some(code as i32),
                            Some(msg),
                        ) {
                            return Err(token_expired_error());
                        }
                        return Err(anyhow!("API 错误: code={}, msg={}", code, msg));
                    }
                }
            }
        }

        let mut full_response = preload_response_snapshot(body);
        if !full_response.is_empty() {
            debug!(
                "SSE response fragments 快照预读: content='{}', 初始长度={}",
                full_response,
                full_response.len()
            );
        }
        let mut message_id: Option<String> = None;
        let mut chunk_count = 0;
        let mut current_event_type: Option<String> = None;

        for line in body.lines() {
            // 解析 event 类型行
            if line.starts_with("event:") {
                current_event_type = Some(line[6..].trim().to_string());
                continue;
            }

            if !line.starts_with("data:") {
                continue;
            }

            let data_str = line[5..].trim();
            if data_str.is_empty() {
                continue;
            }

            // 检查 hint 事件（对话长度上限等错误）
            if current_event_type.as_deref() == Some("hint") {
                if let Ok(hint_data) = serde_json::from_str::<serde_json::Value>(data_str) {
                    let hint_type = hint_data.get("type").and_then(|t| t.as_str());
                    let content = hint_data.get("content").and_then(|c| c.as_str()).unwrap_or("");

                    if hint_type == Some("error") && content.contains("达到对话长度上限") {
                        return Err(anyhow!("SESSION_LIMIT_REACHED: {}", content));
                    }

                    // 其他 hint 错误也记录
                    if hint_type == Some("error") {
                        warn!("DeepSeek hint 错误: {}", content);
                    }
                }
                current_event_type = None;
                continue;
            }

            // 重置事件类型（data 处理完后）
            current_event_type = None;

            chunk_count += 1;

            if let Ok(data) = serde_json::from_str::<serde_json::Value>(data_str) {
                // 提取字段
                let p_field = data.get("p").and_then(|p| p.as_str());
                let o_field = data.get("o").and_then(|o| o.as_str());
                let v_field = data.get("v");

                // 从 ready 事件提取 response_message_id（数字类型）
                if let Some(id) = data.get("response_message_id") {
                    if let Some(id_num) = id.as_i64() {
                        message_id = Some(id_num.to_string());
                    } else if let Some(id_str) = id.as_str() {
                        message_id = Some(id_str.to_string());
                    }
                }

                // 从 response 对象提取 message_id（数字类型）
                if let Some(id) = data
                    .get("v")
                    .and_then(|v| v.get("response"))
                    .and_then(|r| r.get("message_id"))
                {
                    if let Some(id_num) = id.as_i64() {
                        message_id = Some(id_num.to_string());
                    } else if let Some(id_str) = id.as_str() {
                        message_id = Some(id_str.to_string());
                    }
                }

                // 提取文本内容 - 多种格式处理

                // 格式0: response 快照。DeepSeek Web 会先在这里给出已有 fragments，
                // 后续再通过 APPEND/直接输出继续增量推送。
                if let Some(contents) = extract_response_snapshot_content(&data) {
                    if !contents.is_empty() {
                        debug!(
                            "SSE response fragments 快照: content='{}', 累计长度={}",
                            contents,
                            full_response.len()
                        );
                        if full_response.is_empty() || contents.starts_with(&full_response) {
                            full_response = contents;
                        } else if full_response.starts_with(&contents) {
                            debug!("SSE response fragments 快照已包含在当前响应中，忽略");
                        } else {
                            debug!("SSE response fragments 快照与当前响应不连续，保留当前流式响应");
                        }
                    }
                }

                // 格式1: BATCH 操作（包含 fragments 数组）
                // 例: p="response", o="BATCH", v=[{"o":"APPEND","p":"fragments","v":[{"content":"庄",...}]}]
                if p_field == Some("response") && o_field == Some("BATCH") {
                    if let Some(v_array) = v_field.and_then(|v| v.as_array()) {
                        for item in v_array {
                            // 查找 fragments 的 APPEND 操作
                            if item.get("p").and_then(|p| p.as_str()) == Some("fragments") {
                                if let Some(value) = item.get("v") {
                                    let content = collect_fragment_content(value);
                                    if !content.is_empty() {
                                        debug!(
                                            "SSE BATCH/fragments: content='{}', 累计长度={}",
                                            content,
                                            full_response.len()
                                        );
                                        full_response.push_str(&content);
                                    }
                                }
                            }
                        }
                    }
                }
                // 格式2: fragments content 追加
                // 例: p="response/fragments/-1/content", o="APPEND", v="心"
                else if p_field
                    .map(|p| p.contains("fragments") && p.contains("content"))
                    .unwrap_or(false)
                    && o_field == Some("APPEND")
                {
                    if let Some(text) = v_field.and_then(|v| v.as_str()) {
                        debug!(
                            "SSE fragments/content APPEND: v='{}', 累计长度={}",
                            text,
                            full_response.len()
                        );
                        full_response.push_str(text);
                    }
                }
                // 格式3: response/content 格式（R1 模式）
                else if p_field == Some("response/content") {
                    let operation = o_field.unwrap_or("");
                    if let Some(text) = v_field.and_then(|v| v.as_str()) {
                        // 只在 APPEND 或空操作时追加内容（与 chat.js 一致）
                        if operation == "APPEND" || operation.is_empty() {
                            debug!(
                                "SSE response/content APPEND: v='{}', 累计长度={}",
                                text,
                                full_response.len()
                            );
                            full_response.push_str(text);
                        } else {
                            // SET 或其他操作：记录但忽略（chat.js 也不处理 SET）
                            debug!("SSE 忽略操作 '{}': v='{}'", operation, text);
                        }
                    }
                }
                // 格式4: V3 直接输出（无 p 字段）
                else if p_field.is_none() {
                    if let Some(text) = v_field.and_then(|v| v.as_str()) {
                        debug!("SSE V3直接输出: v='{}', 累计长度={}", text, full_response.len());
                        full_response.push_str(text);
                    }
                }
            }
        }

        if full_response.is_empty() {
            return Err(anyhow!(
                "DeepSeek 响应为空，原始响应: {}...",
                &body[..body.len().min(200)]
            ));
        }

        debug!(
            "SSE 解析完成: 共{}个数据块, 响应长度={}",
            chunk_count,
            full_response.len()
        );

        Ok((full_response, message_id))
    }
}

#[cfg(test)]
mod tests {
    use super::DeepSeekWebClient;

    #[test]
    fn parse_sse_response_keeps_initial_response_fragment_snapshot() {
        let client = DeepSeekWebClient::new("token", 10).expect("client should build");
        let body = r#"event: ready
data: {"request_message_id":9,"response_message_id":10,"model_type":"default"}
data: {"v":{"response":{"message_id":10,"fragments":[{"id":2,"type":"RESPONSE","content":"[\"","references":[],"stage_id":1}]}}}
data: {"p":"response/fragments/-1/content","o":"APPEND","v":"Z"}
data: {"v":"HY2020_2024-06-09\", \"三国bigbig_2024-06-09\"]"}
"#;

        let (response, message_id) = client.parse_sse_response(body).expect("sse should parse");

        assert_eq!(message_id, Some("10".to_string()));
        assert_eq!(response, r#"["ZHY2020_2024-06-09", "三国bigbig_2024-06-09"]"#);
    }

    #[test]
    fn parse_sse_response_keeps_nested_batch_fragment_prefix() {
        let client = DeepSeekWebClient::new("token", 10).expect("client should build");
        let body = r#"data: {"p":"response","o":"BATCH","v":[{"p":"fragments","o":"BATCH","v":[{"o":"APPEND","v":[{"id":3,"type":"RESPONSE","content":"[\"","references":[],"stage_id":2}]},{"p":"-2/status","o":"SET","v":"FINISHED"}]},{"p":"has_pending_fragment","o":"SET","v":false}]}
data: {"p":"response/fragments/-1/content","o":"APPEND","v":"Z"}
data: {"v":"HY2020 2024-06-09\", \"三国bigbig 2024-06-09\"]"}
"#;

        let (response, _) = client.parse_sse_response(body).expect("sse should parse");

        assert_eq!(response, r#"["ZHY2020 2024-06-09", "三国bigbig 2024-06-09"]"#);
    }
}

/// 使用 DeepSeek Web API 生成原始响应（不清洗）
///
/// # 参数
/// - `token`: DeepSeek Web Token
/// - `session`: 会话信息（可选，如果为 None 则创建新会话）
/// - `prompt`: 用户消息
/// - `timeout_seconds`: 超时时间
///
/// # 返回
/// - (原始响应文本, 更新后的会话信息)
pub async fn deepseek_web_generate_raw(
    token: &str,
    session: Option<DeepSeekSession>,
    prompt: &str,
    timeout_seconds: u64,
) -> Result<(String, DeepSeekSession)> {
    // 检查并更新 WASM（仅首次调用时执行）
    super::deepseek_pow::check_and_update_wasm().await;

    let client = DeepSeekWebClient::new(token, timeout_seconds)?;

    // 获取或创建会话
    let (session_id, parent_message_id) = match session {
        Some(s) => (s.session_id, s.parent_message_id),
        None => (client.create_session().await?, None),
    };

    // 发送消息
    let (response, new_message_id) = client
        .send_message(&session_id, parent_message_id.as_deref(), prompt, timeout_seconds)
        .await?;

    let updated_session = DeepSeekSession {
        session_id,
        parent_message_id: new_message_id,
    };

    Ok((response, updated_session))
}
