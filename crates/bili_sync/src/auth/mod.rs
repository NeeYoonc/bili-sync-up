use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::bilibili::Credential;
use crate::http::headers::{create_api_headers, create_navigation_headers};

#[derive(Debug, Serialize, Deserialize)]
pub struct QRCodeInfo {
    pub url: String,
    pub qrcode_key: String,
}

#[derive(Debug, Clone)]
pub enum LoginStatus {
    Pending,
    Scanned,
    Confirmed(Box<LoginResult>),
    Expired,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LoginResult {
    pub credential: Credential,
    pub user_info: UserInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserInfo {
    pub user_id: String,
    pub username: String,
    pub avatar_url: String,
}

#[derive(Debug)]
struct QRSession {
    qrcode_key: String,
    created_at: Instant,
}

/// 扫码登录服务
pub struct QRLoginService {
    client: Client,
    sessions: RwLock<HashMap<String, QRSession>>,
}

impl QRLoginService {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .cookie_store(true)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 生成二维码
    pub async fn generate_qr_code(&self) -> Result<(String, QRCodeInfo)> {
        tracing::info!("开始调用B站API生成二维码");

        // 避免会话表无限增长
        self.cleanup_expired_sessions().await;

        // 首先访问B站主页获取 buvid3
        let homepage_url = "https://www.bilibili.com";
        tracing::debug!("发起B站主页访问请求: {}", homepage_url);

        let homepage_request = self.client.get(homepage_url).headers(create_navigation_headers());

        // B站主页访问请求头日志已在建造器时设置

        let _ = homepage_request
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("B站主页访问失败，继续尝试生成二维码 - 错误: {}", e);
                e
            })
            .inspect(|resp| {
                tracing::debug!("B站主页访问成功 - 状态码: {}", resp.status());
                tracing::debug!("B站主页响应头: {:?}", resp.headers());
            });

        // 调用B站API生成二维码
        let qrcode_url = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
        tracing::debug!("发起二维码生成请求: {}", qrcode_url);

        let qrcode_request = self.client.get(qrcode_url).headers(create_api_headers());

        // 二维码生成请求头日志已在建造器时设置

        let response = qrcode_request.send().await;
        let response = match response {
            Ok(resp) => {
                tracing::debug!("二维码生成请求成功 - 状态码: {}, URL: {}", resp.status(), resp.url());
                resp
            }
            Err(e) => {
                tracing::error!("二维码生成请求失败 - 错误: {}", e);
                return Err(e.into());
            }
        };

        let data: serde_json::Value = response.json().await.map_err(|e| {
            tracing::error!("解析B站API响应失败: {}", e);
            e
        })?;

        tracing::debug!("B站API响应数据: {}", data);

        if data["code"].as_i64() != Some(0) {
            let error_msg = format!(
                "B站API返回错误: code={}, message={}",
                data["code"].as_i64().unwrap_or(-1),
                data["message"].as_str().unwrap_or("Unknown error")
            );
            tracing::error!("{}", error_msg);
            return Err(anyhow::anyhow!(error_msg));
        }

        let qr_info = QRCodeInfo {
            url: data["data"]["url"].as_str().unwrap().to_string(),
            qrcode_key: data["data"]["qrcode_key"].as_str().unwrap().to_string(),
        };

        // 生成会话ID并存储
        let session_id = Uuid::new_v4().to_string();
        let session = QRSession {
            qrcode_key: qr_info.qrcode_key.clone(),
            created_at: Instant::now(),
        };

        self.sessions.write().await.insert(session_id.clone(), session);

        // 打印当前存储的所有会话
        {
            let sessions = self.sessions.read().await;
            tracing::debug!("当前会话数量: {}", sessions.len());
            for id in sessions.keys() {
                tracing::debug!("存储的会话ID: {}", id);
            }
        }

        Ok((session_id, qr_info))
    }

    /// 轮询登录状态
    pub async fn poll_login_status(&self, session_id: &str) -> Result<LoginStatus> {
        tracing::debug!("轮询登录状态: session_id={}", session_id);

        // 打印当前存储的所有会话
        {
            let sessions = self.sessions.read().await;
            tracing::debug!("轮询时会话数量: {}", sessions.len());
            for id in sessions.keys() {
                tracing::debug!("存储的会话ID: {}", id);
            }
        }

        let sessions = self.sessions.read().await;
        let session = sessions.get(session_id).ok_or_else(|| {
            tracing::error!("找不到会话: session_id={}", session_id);
            anyhow::anyhow!("Session not found")
        })?;

        // 检查会话是否过期 (3分钟)
        if session.created_at.elapsed().as_secs() > 180 {
            return Ok(LoginStatus::Expired);
        }

        // 调用B站API检查状态
        let poll_url = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
        tracing::debug!(
            "发起扫码状态检查请求: {} - qrcode_key: {}...",
            poll_url,
            &session.qrcode_key[..std::cmp::min(session.qrcode_key.len(), 20)]
        );

        let poll_request = self
            .client
            .get(poll_url)
            .query(&[("qrcode_key", &session.qrcode_key)])
            .headers(create_api_headers());

        // 扫码状态检查请求头日志已在建造器时设置

        let response = poll_request.send().await;
        let response = match response {
            Ok(resp) => {
                tracing::debug!("扫码状态检查请求成功 - 状态码: {}", resp.status());
                resp
            }
            Err(e) => {
                tracing::error!("扫码状态检查请求失败 - 错误: {}", e);
                return Err(e.into());
            }
        };

        // 先提取headers
        let headers = response.headers().clone();
        tracing::debug!("扫码状态检查响应头: {:?}", headers);

        let data: serde_json::Value = match response.json().await {
            Ok(json) => {
                tracing::debug!("扫码状态检查响应解析成功");
                json
            }
            Err(e) => {
                tracing::error!("扫码状态检查响应解析失败 - 错误: {}", e);
                return Err(e.into());
            }
        };

        match data["data"]["code"].as_i64() {
            Some(0) => {
                // 登录成功，提取凭证
                let cookies = self.extract_cookies_from_headers(&headers)?;

                // 从登录响应中提取 buvid3 和 buvid4
                let mut buvid3 = cookies.get("buvid3").cloned().unwrap_or_default();
                let mut buvid4 = cookies.get("buvid4").cloned();

                let user_info = self.get_user_info(&cookies).await?;

                // 如果还是没有 buvid3 或 buvid4，尝试从之前访问主页时获取的cookie中查找
                if buvid3.is_empty() || buvid4.is_none() {
                    tracing::debug!(
                        "登录响应未包含 buvid3/buvid4（buvid3_missing={}, buvid4_missing={}），将尝试从现有配置或 spi 接口获取",
                        buvid3.is_empty(),
                        buvid4.is_none()
                    );

                    // 从当前配置中获取（如果有的话）
                    let current_config = crate::config::reload_config();
                    if let Some(current_cred) = current_config.credential.load().as_ref() {
                        if buvid3.is_empty() && !current_cred.buvid3.is_empty() {
                            buvid3 = current_cred.buvid3.clone();
                            tracing::debug!("使用现有配置中的 buvid3");
                        }
                        if buvid4.is_none() {
                            if let Some(ref existing_buvid4) = current_cred.buvid4 {
                                if !existing_buvid4.is_empty() {
                                    buvid4 = Some(existing_buvid4.clone());
                                    tracing::debug!("使用现有配置中的 buvid4");
                                }
                            }
                        }
                    }

                    // 如果还是没有，尝试生成新的 buvid3 和 buvid4
                    if buvid3.is_empty() || buvid4.is_none() {
                        match self.generate_buvids().await {
                            Ok((new_buvid3, new_buvid4)) => {
                                if buvid3.is_empty() {
                                    buvid3 = new_buvid3;
                                    tracing::debug!("成功生成新的 buvid3");
                                }
                                if buvid4.is_none() {
                                    buvid4 = new_buvid4;
                                    if let Some(ref b4) = buvid4 {
                                        tracing::debug!("成功生成新的 buvid4: {}", b4);
                                    } else {
                                        tracing::warn!("未能获取 buvid4");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("生成 buvids 失败: {}，将使用现有值", e);
                            }
                        }
                    }
                }

                let credential = Credential {
                    sessdata: cookies.get("SESSDATA").unwrap().clone(),
                    bili_jct: cookies.get("bili_jct").unwrap().clone(),
                    buvid3: buvid3.clone(),
                    dedeuserid: cookies.get("DedeUserID").unwrap().clone(),
                    ac_time_value: data["data"]["refresh_token"].as_str().unwrap_or("").to_string(),
                    buvid4,
                    dedeuserid_ckmd5: cookies.get("DedeUserID__ckMd5").cloned(),
                };

                let login_result = LoginResult { credential, user_info };

                Ok(LoginStatus::Confirmed(Box::new(login_result)))
            }
            Some(86038) => Ok(LoginStatus::Expired),
            Some(86090) => Ok(LoginStatus::Scanned),
            Some(86101) => Ok(LoginStatus::Pending),
            _ => Ok(LoginStatus::Error(
                data["data"]["message"].as_str().unwrap_or("Unknown error").to_string(),
            )),
        }
    }

    /// 从响应头中提取Cookie
    fn extract_cookies_from_headers(&self, headers: &reqwest::header::HeaderMap) -> Result<HashMap<String, String>> {
        let mut cookies = HashMap::new();

        // 从响应头中提取Set-Cookie
        for header_value in headers.get_all("set-cookie").iter() {
            if let Ok(cookie_str) = header_value.to_str() {
                // 解析cookie
                for cookie_part in cookie_str.split(';') {
                    if let Some((key, value)) = cookie_part.split_once('=') {
                        let key = key.trim();
                        let value = value.trim();

                        if [
                            "SESSDATA",
                            "bili_jct",
                            "DedeUserID",
                            "DedeUserID__ckMd5",
                            "buvid3",
                            "buvid4",
                        ]
                        .contains(&key)
                        {
                            cookies.insert(key.to_string(), value.to_string());
                        }
                    }
                }
            }
        }

        // 确保必要的cookie都存在
        let required_cookies = ["SESSDATA", "bili_jct", "DedeUserID"];
        for &required in &required_cookies {
            if !cookies.contains_key(required) {
                return Err(anyhow::anyhow!("Missing required cookie: {}", required));
            }
        }

        Ok(cookies)
    }

    /// 获取用户信息
    async fn get_user_info(&self, cookies: &HashMap<String, String>) -> Result<UserInfo> {
        // 构建cookie字符串
        let cookie_str = cookies
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("; ");

        let response = self
            .client
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header("Cookie", cookie_str)
            .send()
            .await?;

        let data: serde_json::Value = response.json().await?;

        if data["code"].as_i64() != Some(0) {
            return Err(anyhow::anyhow!(
                "Failed to get user info: {}",
                data["message"].as_str().unwrap_or("Unknown error")
            ));
        }

        let user_data = &data["data"];
        Ok(UserInfo {
            user_id: user_data["mid"].as_i64().unwrap_or(0).to_string(),
            username: user_data["uname"].as_str().unwrap_or("").to_string(),
            avatar_url: user_data["face"].as_str().unwrap_or("").to_string(),
        })
    }

    /// 清理过期会话
    pub async fn cleanup_expired_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        sessions.retain(|_, session| {
            session.created_at.elapsed().as_secs() < 300 // 5分钟
        });
    }

    /// 生成 buvid3 和 buvid4
    async fn generate_buvids(&self) -> Result<(String, Option<String>)> {
        tracing::debug!("尝试生成 buvid3 和 buvid4...");

        // 方法1：访问 B站的 buvid3/buvid4 生成接口
        let response = self
            .client
            .get("https://api.bilibili.com/x/frontend/finger/spi")
            .header("Referer", "https://www.bilibili.com")
            .header("Origin", "https://www.bilibili.com")
            .send()
            .await?;

        let data: serde_json::Value = response.json().await?;

        if data["code"].as_i64() == Some(0) {
            let buvid3 = data["data"]["b_3"].as_str();
            let buvid4 = data["data"]["b_4"].as_str();

            if let Some(buvid3) = buvid3 {
                tracing::debug!("从 spi 接口获取到 buvid3: {}", buvid3);
                if let Some(buvid4) = buvid4 {
                    tracing::debug!("从 spi 接口获取到 buvid4: {}", buvid4);
                    return Ok((buvid3.to_string(), Some(buvid4.to_string())));
                } else {
                    tracing::warn!("spi 接口未返回 buvid4");
                    return Ok((buvid3.to_string(), None));
                }
            }
        }

        // 方法2：如果上面失败，尝试生成一个随机的 buvid3
        // B站的 buvid3 格式通常是：大写字母和数字的组合，长度约35个字符
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let charset: &[u8] = b"0123456789ABCDEF";
        let buvid3: String = (0..35)
            .map(|_| {
                let idx = rng.gen_range(0..charset.len());
                charset[idx] as char
            })
            .collect();

        tracing::debug!("生成随机 buvid3: {}", buvid3);
        tracing::warn!("无法获取 buvid4，将使用空值");
        Ok((buvid3, None))
    }
}

impl Default for QRLoginService {
    fn default() -> Self {
        Self::new()
    }
}
