use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwapOption;
use leaky_bucket::RateLimiter;
use reqwest::StatusCode;
use reqwest::{header, Method};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::bilibili::credential::WbiImg;
use crate::bilibili::{Credential, Validate};
use crate::config::RateLimit;

#[derive(Debug, Clone)]
pub struct UserFollowingInfo {
    pub mid: i64,
    pub name: String,
    pub face: String,
    pub sign: String,
    pub official_verify: Option<UserOfficialVerify>,
    pub follower: Option<i64>, // 粉丝数（关注列表API不返回此字段，需单独获取）
}

#[derive(Debug, Clone)]
pub struct UserOfficialVerify {
    pub type_: i32,
    pub desc: String,
}

/// bilibili搜索响应包装
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponseWrapper {
    pub results: Vec<SearchResult>,
    pub total: u32,
    pub num_pages: u32,
}

/// bilibili搜索结果类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub result_type: String,       // video, bili_user, media_bangumi等
    pub title: String,             // 标题
    pub author: String,            // 作者/UP主
    pub bvid: Option<String>,      // 视频BV号
    pub aid: Option<i64>,          // 视频AV号
    pub mid: Option<i64>,          // UP主ID
    pub season_id: Option<String>, // 番剧season_id
    pub media_id: Option<String>,  // 番剧media_id
    pub cover: String,             // 封面图
    pub description: String,       // 描述
    pub duration: Option<String>,  // 视频时长
    pub pubdate: Option<i64>,      // 发布时间
    pub play: Option<i64>,         // 播放量
    pub danmaku: Option<i64>,      // 弹幕数
    pub follower: Option<i64>,     // 粉丝数（UP主搜索结果）
}

/// bilibili搜索响应
#[derive(Debug, Deserialize)]
struct SearchResponse {
    code: i32,
    message: String,
    data: Option<SearchData>,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    result: Option<Vec<Value>>,
    #[serde(rename = "numPages")]
    num_pages: Option<i32>,
    #[serde(rename = "numResults")]
    num_results: Option<i32>,
}

// 一个对 reqwest::Client 的简单封装，用于 Bilibili 请求
#[derive(Clone)]
pub struct Client(reqwest::Client);

impl Client {
    pub fn new() -> Self {
        // 正常访问 api 所必须的 header，作为默认 header 添加到每个请求中
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
            ),
        );
        headers.insert(
            header::REFERER,
            header::HeaderValue::from_static("https://www.bilibili.com"),
        );
        Self(
            reqwest::Client::builder()
                .default_headers(headers)
                .gzip(true)
                .referer(false)  // 禁用自动Referer策略，使用我们自己设置的Referer头
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
        )
    }

    // a wrapper of reqwest::Client::request to add credential to the request
    pub fn request(&self, method: Method, url: &str, credential: Option<&Credential>) -> reqwest::RequestBuilder {
        self.request_with_gaia_vtoken(method, url, credential, None)
    }

    // a wrapper of reqwest::Client::request to add credential and gaia_vtoken to the request
    pub fn request_with_gaia_vtoken(
        &self,
        method: Method,
        url: &str,
        credential: Option<&Credential>,
        gaia_vtoken: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut req = self.0.request(method, url);
        // 如果有 credential，会将其转换成 cookie 添加到请求的 header 中
        if let Some(credential) = credential {
            let mut cookie_parts = vec![
                format!("SESSDATA={}", credential.sessdata),
                format!("bili_jct={}", credential.bili_jct),
                format!("buvid3={}", credential.buvid3),
                format!("DedeUserID={}", credential.dedeuserid),
                format!("ac_time_value={}", credential.ac_time_value),
            ];

            if let Some(buvid4) = &credential.buvid4 {
                cookie_parts.push(format!("buvid4={}", buvid4));
            }

            if let Some(ckmd5) = &credential.dedeuserid_ckmd5 {
                cookie_parts.push(format!("DedeUserID__ckMd5={}", ckmd5));
            }

            let cookie_str = cookie_parts.join("; ");

            // 调试日志：不要输出敏感Cookie内容（SESSDATA/bili_jct等）
            tracing::debug!(
                "发送Cookie字段: count={}, has_buvid4={}, has_ckmd5={}, sessdata_len={}, bili_jct_len={}, buvid3_len={}, dedeuserid_len={}, ac_time_value_len={}",
                cookie_parts.len(),
                credential.buvid4.is_some(),
                credential.dedeuserid_ckmd5.is_some(),
                credential.sessdata.len(),
                credential.bili_jct.len(),
                credential.buvid3.len(),
                credential.dedeuserid.len(),
                credential.ac_time_value.len(),
            );

            req = req.header(header::COOKIE, cookie_str);
        }

        // 如果有gaia_vtoken，添加到请求头中
        if let Some(token) = gaia_vtoken {
            req = req.header("x-gaia-vtoken", token);
            tracing::debug!("已添加gaia_vtoken到请求头 (len={})", token.len());
        }

        req
    }
}

// clippy 建议实现 Default trait
impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

fn response_body_preview(text: &str) -> String {
    text.chars()
        .take(200)
        .collect::<String>()
        .replace('\r', " ")
        .replace('\n', " ")
}

#[derive(Debug, Clone, Copy)]
enum SearchCredentialState {
    Missing,
    Incomplete,
    Complete,
}

fn current_search_credential_state() -> SearchCredentialState {
    let config = crate::config::reload_config();
    let credential = config.credential.load();
    match credential.as_deref() {
        Some(credential)
            if !credential.sessdata.trim().is_empty()
                && !credential.buvid3.trim().is_empty()
                && !credential.dedeuserid.trim().is_empty() =>
        {
            SearchCredentialState::Complete
        }
        Some(_) => SearchCredentialState::Incomplete,
        None => SearchCredentialState::Missing,
    }
}

fn search_http_status_message(operation: &str, status: StatusCode) -> String {
    if status == StatusCode::PRECONDITION_FAILED {
        match current_search_credential_state() {
            SearchCredentialState::Complete => format!(
                "{}触发 B 站风控(412)：程序已携带当前配置的 Cookie/buvid 指纹并使用搜索页 Referer，但仍被 B 站安全策略拒绝；请稍后重试，或检查这套凭据/出口 IP 是否已被风控",
                operation
            ),
            SearchCredentialState::Incomplete => format!(
                "{}触发 B 站风控(412)：当前 B 站凭据缺少 SESSDATA、buvid3 或 DedeUserID，搜索请求无法形成稳定 Cookie/buvid 指纹；请重新扫码或补齐整套凭据",
                operation
            ),
            SearchCredentialState::Missing => format!(
                "{}触发 B 站风控(412)：当前未配置 B 站凭据，搜索请求无法携带 Cookie/buvid 指纹；请先扫码配置 B 站凭据后再试",
                operation
            ),
        }
    } else {
        format!("{}请求失败: {}", operation, status)
    }
}

async fn decode_json_response<T>(response: reqwest::Response, operation: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let body = response.text().await.with_context(|| {
        format!(
            "读取{}响应体失败: status={}, content-type={}",
            operation, status, content_type
        )
    })?;

    serde_json::from_str(&body).with_context(|| {
        format!(
            "{}响应不是有效 JSON: status={}, content-type={}, body_prefix={}",
            operation,
            status,
            content_type,
            response_body_preview(&body)
        )
    })
}

#[derive(Clone)]
pub struct BiliClient {
    pub client: Client,
    limiter: Option<Arc<RateLimiter>>,
    /// 缓存的gaia_vtoken，用于绕过风控
    gaia_vtoken: Arc<ArcSwapOption<String>>,
}

impl BiliClient {
    pub fn new(_cookie: String) -> Self {
        let client = Client::new();
        let config = crate::config::reload_config();
        let limiter = config
            .concurrent_limit
            .rate_limit
            .as_ref()
            .map(|RateLimit { limit, duration }| {
                Arc::new(
                    RateLimiter::builder()
                        .initial(*limit)
                        .refill(*limit)
                        .max(*limit)
                        .interval(Duration::from_millis(*duration))
                        .build(),
                )
            });
        Self {
            client,
            limiter,
            gaia_vtoken: Arc::new(ArcSwapOption::empty()),
        }
    }

    /// 获取当前用户ID的辅助函数
    fn get_current_user_id(&self) -> Result<i64, anyhow::Error> {
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        match credential.as_ref() {
            Some(cred) => cred.dedeuserid.parse::<i64>().map_err(|_| anyhow!("无效的用户ID")),
            None => Err(anyhow!("未设置登录凭据")),
        }
    }

    /// 获取一个预构建的请求，通过该方法获取请求时会检查并等待速率限制
    pub async fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        if let Some(limiter) = &self.limiter {
            limiter.acquire_one().await;
        }
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        let gaia_vtoken = self.get_gaia_vtoken();
        self.client
            .request_with_gaia_vtoken(method, url, credential.as_deref(), gaia_vtoken.as_deref())
    }

    /// 获取一个不附带登录凭证的公开请求
    pub async fn public_request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        if let Some(limiter) = &self.limiter {
            limiter.acquire_one().await;
        }
        self.client.request(method, url, None)
    }

    /// 发送 GET 请求
    pub async fn get(&self, url: &str, token: CancellationToken) -> Result<reqwest::Response> {
        if let Some(limiter) = &self.limiter {
            tokio::select! {
                biased;
                _ = token.cancelled() => return Err(anyhow!("Request cancelled in limiter")),
                _ = limiter.acquire_one() => {},
            }
        }
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        let request_builder = self.client.request(Method::GET, url, credential.as_deref());

        let response = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(anyhow!("Request cancelled before send")),
            res = request_builder.send() => res,
        };

        Ok(response?)
    }

    /// 发送不附带登录凭证的公开 GET 请求
    pub async fn public_get(&self, url: &str, token: CancellationToken) -> Result<reqwest::Response> {
        if let Some(limiter) = &self.limiter {
            tokio::select! {
                biased;
                _ = token.cancelled() => return Err(anyhow!("Request cancelled in limiter")),
                _ = limiter.acquire_one() => {},
            }
        }

        let request_builder = self.client.request(Method::GET, url, None);

        let response = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(anyhow!("Request cancelled before send")),
            res = request_builder.send() => res,
        };

        Ok(response?)
    }

    pub async fn check_refresh(&self) -> Result<()> {
        self.refresh_credential(false).await.map(|_| ())
    }

    pub async fn refresh_credential(&self, force: bool) -> Result<bool> {
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        let Some(credential) = credential.as_deref() else {
            return Ok(false);
        };

        let refresh_info = match credential.refresh_info(&self.client).await {
            Ok(refresh_info) => refresh_info,
            Err(err) => {
                let error_text = format!("{:#}", err);
                let is_login_expired = error_text.contains("status code: -101") || error_text.contains("账号未登录");
                if is_login_expired && !credential.ac_time_value.trim().is_empty() {
                    warn!("检测到凭证已过期，跳过预检查并直接尝试刷新凭证");
                    crate::bilibili::credential::CookieRefreshInfo {
                        need_refresh: true,
                        timestamp: None,
                    }
                } else {
                    return Err(err);
                }
            }
        };

        if !force && !refresh_info.need_refresh {
            return Ok(false);
        }

        let new_credential = credential.refresh(&self.client, refresh_info.timestamp).await?;
        config.credential.store(Some(Arc::new(new_credential.clone())));

        self.persist_refreshed_credential(&config).await?;
        info!("credential已刷新并保存到数据库");

        Ok(true)
    }

    async fn persist_refreshed_credential(&self, config: &crate::config::Config) -> Result<()> {
        let manager = crate::config::get_config_manager().context("配置管理器未初始化，无法保存刷新后的 credential")?;
        let credential_json = serde_json::to_value(&config.credential).context("序列化刷新后的 credential 失败")?;
        manager
            .update_config_item("credential", credential_json)
            .await
            .context("保存刷新后的 credential 到数据库失败")?;
        crate::config::reload_config_bundle()
            .await
            .context("刷新 credential 后重新加载配置失败")?;
        Ok(())
    }

    /// 获取 wbi img，用于生成请求签名
    pub async fn wbi_img(&self) -> Result<WbiImg> {
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        let mut res = self
            .client
            .request(
                Method::GET,
                "https://api.bilibili.com/x/web-interface/nav",
                credential.as_deref(),
            )
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;
        Ok(serde_json::from_value(res["data"]["wbi_img"].take())?)
    }

    /// 搜索bilibili内容
    ///
    /// # Arguments
    /// * `keyword` - 搜索关键词
    /// * `search_type` - 搜索类型：video(视频), bili_user(UP主), media_bangumi(番剧)等
    /// * `page` - 页码（从1开始）
    /// * `page_size` - 每页数量
    pub async fn search(
        &self,
        keyword: &str,
        search_type: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResponseWrapper> {
        match self.search_via_legacy_type(keyword, search_type, page, page_size).await {
            Ok(wrapper) => return Ok(wrapper),
            Err(err) => {
                warn!("旧搜索接口失败，回退 WBI 搜索接口: {:#}", err);
                let legacy_error = err;
                match self.search_via_wbi_type(keyword, search_type, page, page_size).await {
                    Ok(wrapper) => return Ok(wrapper),
                    Err(err) => {
                        warn!("WBI 搜索接口失败，最后回退 all/v2 搜索接口: {:#}", err);
                        return self
                            .search_via_all_v2(keyword, search_type, page, page_size)
                            .await
                            .with_context(|| {
                                format!("旧搜索接口失败: {:#}; WBI 搜索接口失败: {:#}", legacy_error, err)
                            });
                    }
                }
            }
        }
    }

    /// 使用原有搜索接口，失败时由调用方回退到 WBI 搜索。
    async fn search_via_legacy_type(
        &self,
        keyword: &str,
        search_type: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResponseWrapper> {
        let url = "https://api.bilibili.com/x/web-interface/search/type";

        let params = [
            ("keyword", keyword),
            ("search_type", search_type),
            ("page", &page.to_string()),
            ("page_size", &page_size.to_string()),
            ("order", "totalrank"), // 按综合排序
            ("duration", "0"),      // 不限时长
            ("tids", "0"),          // 不限分区
        ];

        let response = self
            .request(Method::GET, url)
            .await
            .header(header::REFERER, "https://search.bilibili.com/")
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!(search_http_status_message("旧搜索接口", response.status())));
        }

        let search_response: SearchResponse = decode_json_response(response, "旧搜索").await?;
        self.parse_typed_search_response(search_response, search_type, page_size)
    }

    /// 原有搜索接口失败时，回退到 B 站 Web 端实际调用的 WBI 搜索接口。
    async fn search_via_wbi_type(
        &self,
        keyword: &str,
        search_type: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResponseWrapper> {
        let url = "https://api.bilibili.com/x/web-interface/wbi/search/type";
        let wbi_img = self.wbi_img().await.context("获取 WBI 签名参数失败")?;
        let params = HashMap::from([
            ("category_id".to_string(), String::new()),
            ("search_type".to_string(), search_type.to_string()),
            ("ad_resource".to_string(), "5646".to_string()),
            ("__refresh__".to_string(), "true".to_string()),
            ("_extra".to_string(), String::new()),
            ("context".to_string(), String::new()),
            ("page".to_string(), page.to_string()),
            ("page_size".to_string(), page_size.to_string()),
            ("order".to_string(), String::new()),
            ("pubtime_begin_s".to_string(), "0".to_string()),
            ("pubtime_end_s".to_string(), "0".to_string()),
            ("duration".to_string(), String::new()),
            ("from_source".to_string(), String::new()),
            ("from_spmid".to_string(), "333.337".to_string()),
            ("platform".to_string(), "pc".to_string()),
            ("highlight".to_string(), "1".to_string()),
            ("single_column".to_string(), "0".to_string()),
            ("keyword".to_string(), keyword.to_string()),
            ("source_tag".to_string(), "3".to_string()),
            ("gaia_vtoken".to_string(), String::new()),
            ("order_sort".to_string(), "0".to_string()),
            ("user_type".to_string(), "0".to_string()),
            ("dynamic_offset".to_string(), "0".to_string()),
            ("web_location".to_string(), "1430654".to_string()),
        ]);
        let signed_params = wbi_img.sign_params(params).await.context("生成搜索 WBI 签名失败")?;

        let response = self
            .request(Method::GET, url)
            .await
            .header(header::REFERER, "https://search.bilibili.com/")
            .query(&signed_params)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!(search_http_status_message("WBI 搜索接口", response.status())));
        }

        let search_response: SearchResponse = decode_json_response(response, "WBI 搜索").await?;
        self.parse_typed_search_response(search_response, search_type, page_size)
    }

    fn parse_typed_search_response(
        &self,
        search_response: SearchResponse,
        search_type: &str,
        page_size: u32,
    ) -> Result<SearchResponseWrapper> {
        if search_response.code != 0 {
            return Err(anyhow!("搜索API返回错误: {}", search_response.message));
        }

        let data = search_response.data.unwrap_or(SearchData {
            result: None,
            num_pages: None,
            num_results: None,
        });
        let results = data.result.unwrap_or_default();
        let total = data.num_results.unwrap_or(0) as u32;
        let mut num_pages = data.num_pages.unwrap_or(0) as u32;
        if num_pages == 0 {
            num_pages = if total > 0 { total.div_ceil(page_size) } else { 1 };
        }
        let mut parsed_results = Vec::new();

        for item in results {
            if let Ok(result) = self.parse_search_result(&item, search_type) {
                parsed_results.push(result);
            }
        }

        Ok(SearchResponseWrapper {
            results: parsed_results,
            total,
            num_pages,
        })
    }

    /// WBI 搜索也失败时，最后回退 all/v2 搜索接口。
    async fn search_via_all_v2(
        &self,
        keyword: &str,
        search_type: &str,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResponseWrapper> {
        let url = "https://api.bilibili.com/x/web-interface/search/all/v2";
        let params = [
            ("keyword", keyword),
            ("page", &page.to_string()),
            ("page_size", &page_size.to_string()),
        ];

        let response = self
            .request(Method::GET, url)
            .await
            .header(header::REFERER, "https://search.bilibili.com/")
            .query(&params)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(anyhow!(search_http_status_message(
                "all/v2 搜索接口",
                response.status()
            )));
        }

        let payload: serde_json::Value = decode_json_response(response, "all/v2 搜索").await?;
        let code = payload["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            let msg = payload["message"].as_str().unwrap_or("unknown");
            return Err(anyhow!("all/v2 搜索API返回错误: {}", msg));
        }

        let data = &payload["data"];
        let pageinfo = &data["pageinfo"][search_type];
        let total = pageinfo["numResults"]
            .as_u64()
            .or_else(|| pageinfo["total"].as_u64())
            .unwrap_or(0) as u32;
        let mut num_pages = pageinfo["pages"].as_u64().unwrap_or(0) as u32;
        if num_pages == 0 {
            num_pages = data["numPages"].as_u64().unwrap_or(0) as u32;
        }
        if num_pages == 0 {
            num_pages = if total > 0 { total.div_ceil(page_size) } else { 1 };
        }

        let mut parsed_results = Vec::new();
        let type_block = data["result"].as_array().and_then(|arr| {
            arr.iter()
                .find(|item| item["result_type"].as_str() == Some(search_type))
        });
        let items = type_block.and_then(|block| block["data"].as_array());

        if let Some(items) = items {
            for item in items {
                if let Ok(result) = self.parse_search_result(item, search_type) {
                    parsed_results.push(result);
                }
            }
        }

        Ok(SearchResponseWrapper {
            results: parsed_results,
            total,
            num_pages,
        })
    }

    /// 解析搜索结果
    fn parse_search_result(&self, item: &Value, search_type: &str) -> Result<SearchResult> {
        match search_type {
            "video" => {
                // 解析视频搜索结果
                Ok(SearchResult {
                    result_type: "video".to_string(),
                    title: item["title"].as_str().unwrap_or("").to_string(),
                    author: item["author"].as_str().unwrap_or("").to_string(),
                    bvid: item["bvid"].as_str().map(|s| s.to_string()),
                    aid: item["aid"].as_i64(),
                    mid: item["mid"].as_i64(),
                    season_id: None,
                    media_id: None,
                    cover: item["pic"].as_str().unwrap_or("").to_string(),
                    description: item["description"].as_str().unwrap_or("").to_string(),
                    duration: item["duration"].as_str().map(|s| s.to_string()),
                    pubdate: item["pubdate"].as_i64(),
                    play: item["play"].as_i64(),
                    danmaku: item["video_review"].as_i64(),
                    follower: None,
                })
            }
            "bili_user" => {
                // 解析UP主搜索结果
                Ok(SearchResult {
                    result_type: "bili_user".to_string(),
                    title: item["uname"].as_str().unwrap_or("").to_string(),
                    author: item["uname"].as_str().unwrap_or("").to_string(),
                    bvid: None,
                    aid: None,
                    mid: item["mid"].as_i64(),
                    season_id: None,
                    media_id: None,
                    cover: item["upic"].as_str().unwrap_or("").to_string(),
                    description: item["usign"].as_str().unwrap_or("").to_string(),
                    duration: None,
                    pubdate: None,
                    play: None,
                    danmaku: None,
                    follower: item["fans"].as_i64(),
                })
            }
            "media_bangumi" => {
                // 解析番剧搜索结果
                Ok(SearchResult {
                    result_type: "media_bangumi".to_string(),
                    title: item["title"].as_str().unwrap_or("").to_string(),
                    author: item["staff"].as_str().unwrap_or("").to_string(),
                    bvid: None,
                    aid: None,
                    mid: None,
                    season_id: item["season_id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| item["season_id"].as_i64().map(|s| s.to_string())),
                    media_id: item["media_id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| item["media_id"].as_i64().map(|s| s.to_string())),
                    cover: item["cover"].as_str().unwrap_or("").to_string(),
                    description: item["desc"].as_str().unwrap_or("").to_string(),
                    duration: None,
                    pubdate: None,
                    play: None,
                    danmaku: None,
                    follower: None,
                })
            }
            "media_ft" => {
                // 解析影视搜索结果（电影、电视剧等）
                Ok(SearchResult {
                    result_type: "media_ft".to_string(),
                    title: item["title"].as_str().unwrap_or("").to_string(),
                    author: item["staff"].as_str().unwrap_or("").to_string(),
                    bvid: None,
                    aid: None,
                    mid: None,
                    season_id: item["season_id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| item["season_id"].as_i64().map(|s| s.to_string())),
                    media_id: item["media_id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| item["media_id"].as_i64().map(|s| s.to_string())),
                    cover: item["cover"].as_str().unwrap_or("").to_string(),
                    description: item["desc"].as_str().unwrap_or("").to_string(),
                    duration: None,
                    pubdate: None,
                    play: None,
                    danmaku: None,
                    follower: None,
                })
            }
            _ => Err(anyhow!("不支持的搜索类型: {}", search_type)),
        }
    }

    /// 获取用户创建的收藏夹列表
    pub async fn get_user_favorite_folders(
        &self,
        uid: Option<i64>,
    ) -> Result<Vec<crate::api::UserFavoriteFolder>, anyhow::Error> {
        let uid = match uid {
            Some(uid) => uid,
            None => self.get_current_user_id()?,
        };

        let response = self
            .request(
                reqwest::Method::GET,
                "https://api.bilibili.com/x/v3/fav/folder/created/list-all",
            )
            .await
            .query(&[("up_mid", uid.to_string().as_str())])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;

        // 检查list字段是否存在且不为null
        let list_value = &response["data"]["list"];
        if list_value.is_null() {
            // 如果list为null，返回空列表
            return Ok(Vec::new());
        }

        let folders: Vec<crate::api::UserFavoriteFolder> =
            serde_json::from_value(list_value.clone()).context("解析收藏夹列表失败")?;

        Ok(folders)
    }

    /// 获取UP主的合集和系列列表（完整分页版本）
    pub async fn get_user_collections(
        &self,
        mid: i64,
        _page: u32,
        page_size: u32,
    ) -> Result<crate::api::response::UserCollectionsResponse> {
        let normalized_page_size = page_size.clamp(1, 30);
        let mut page_size_candidates = vec![normalized_page_size];
        for fallback_page_size in [20, 10] {
            if normalized_page_size > fallback_page_size {
                page_size_candidates.push(fallback_page_size);
            }
        }

        let mut last_err = None;
        for (index, candidate_page_size) in page_size_candidates.iter().copied().enumerate() {
            let is_final_candidate = index + 1 == page_size_candidates.len();
            match self
                .get_user_collections_with_page_size(mid, candidate_page_size, is_final_candidate)
                .await
            {
                Ok(response) => return Ok(response),
                Err(err) => {
                    last_err = Some(err);
                    if let Some(next_page_size) = page_size_candidates.get(index + 1) {
                        info!(
                            "UP主 {} 使用 page_size={} 获取合集失败，将降级重试为 page_size={}",
                            mid, candidate_page_size, next_page_size
                        );
                    }
                }
            }
        }

        if let Some(ref err) = last_err {
            warn!(
                "UP主 {} 获取合集失败，已依次尝试 page_size={:?}: {}",
                mid, page_size_candidates, err
            );
        }

        Err(last_err.unwrap_or_else(|| anyhow!("获取合集失败")))
    }

    async fn get_user_collections_with_page_size(
        &self,
        mid: i64,
        page_size: u32,
        emit_warning: bool,
    ) -> Result<crate::api::response::UserCollectionsResponse> {
        use serde_json::Value;

        // 同时获取合集(seasons)和系列(series)
        let mut all_collections = Vec::new();
        let seasons_url = "https://api.bilibili.com/x/polymer/web-space/seasons_series_list";

        // 循环获取所有页面的数据
        let mut current_page = 1u32;
        let mut total_count = 0u32;

        loop {
            let mid_str = mid.to_string();
            let page_num_str = current_page.to_string();
            let page_size_str = page_size.to_string();
            let web_location = "333.1387".to_string();
            let seasons_response = match self
                .request(Method::GET, seasons_url)
                .await
                .query(&[
                    ("mid", &mid_str),
                    ("page_num", &page_num_str),
                    ("page_size", &page_size_str),
                    ("web_location", &web_location),
                ])
                .send()
                .await
            {
                Ok(response) => match response.error_for_status() {
                    Ok(response) => match response.json::<Value>().await {
                        Ok(json) => match json.validate() {
                            Ok(validated) => validated,
                            Err(e) => {
                                if emit_warning {
                                    warn!("UP主 {} 合集响应验证失败: {}", mid, e);
                                } else {
                                    info!("UP主 {} 合集响应验证失败: {}", mid, e);
                                }
                                return Err(e.context("合集响应验证失败"));
                            }
                        },
                        Err(e) => {
                            if emit_warning {
                                warn!("UP主 {} 合集JSON解析失败: {}", mid, e);
                            } else {
                                info!("UP主 {} 合集JSON解析失败: {}", mid, e);
                            }
                            return Err(anyhow!("解析合集响应JSON失败: {}", e));
                        }
                    },
                    Err(e) => {
                        if emit_warning {
                            warn!("UP主 {} 合集请求状态错误: {}", mid, e);
                        } else {
                            info!("UP主 {} 合集请求状态错误: {}", mid, e);
                        }
                        return Err(anyhow!("合集请求返回错误状态: {}", e));
                    }
                },
                Err(e) => {
                    if emit_warning {
                        warn!("UP主 {} 合集请求失败: {}", mid, e);
                    } else {
                        info!("UP主 {} 合集请求失败: {}", mid, e);
                    }
                    return Err(anyhow!("发送合集请求失败: {}", e));
                }
            };

            // 检查响应是否包含items_lists字段
            if seasons_response["data"]["items_lists"].is_null() {
                warn!("UP主 {} 的合集响应中 items_lists 为 null，可能需要登录权限", mid);
                break;
            }

            // 获取总数（只在第一页时获取）
            if current_page == 1 {
                total_count = seasons_response["data"]["page"]["total"].as_i64().unwrap_or(0) as u32;
                debug!("UP主 {} 总共有 {} 个合集", mid, total_count);
            }

            let mut current_page_collections = Vec::new();

            // 解析合集数据 (seasons)
            if let Some(seasons_list) = seasons_response["data"]["items_lists"]["seasons_list"].as_array() {
                for season in seasons_list {
                    if let Some(season_obj) = season.as_object() {
                        // 从不同的可能位置尝试获取封面，优先从根级cover字段获取
                        let cover = season_obj
                            .get("cover")
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                season_obj
                                    .get("meta")
                                    .and_then(|meta| meta.get("cover"))
                                    .and_then(|v| v.as_str())
                            })
                            .or_else(|| {
                                season_obj
                                    .get("meta")
                                    .and_then(|meta| meta.get("square_cover"))
                                    .and_then(|v| v.as_str())
                            })
                            .or_else(|| {
                                season_obj
                                    .get("meta")
                                    .and_then(|meta| meta.get("horizontal_cover"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("")
                            .to_string();

                        current_page_collections.push(crate::api::response::UserCollection {
                            collection_type: "season".to_string(),
                            sid: season_obj
                                .get("meta")
                                .and_then(|meta| meta.get("season_id"))
                                .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                                .unwrap_or(0)
                                .to_string(),
                            name: season_obj
                                .get("meta")
                                .and_then(|meta| meta.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            cover,
                            description: season_obj
                                .get("meta")
                                .and_then(|meta| meta.get("description"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            total: season_obj
                                .get("meta")
                                .and_then(|meta| meta.get("total"))
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0),
                            ptime: season_obj
                                .get("meta")
                                .and_then(|meta| meta.get("ptime"))
                                .and_then(|v| v.as_i64()),
                            mid,
                        });
                    }
                }
            }

            // 解析系列数据 (series)
            if let Some(series_list) = seasons_response["data"]["items_lists"]["series_list"].as_array() {
                for series in series_list {
                    if let Some(series_obj) = series.as_object() {
                        // 从不同的可能位置尝试获取封面，优先从根级cover字段获取
                        let cover = series_obj
                            .get("cover")
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                series_obj
                                    .get("meta")
                                    .and_then(|meta| meta.get("cover"))
                                    .and_then(|v| v.as_str())
                            })
                            .or_else(|| {
                                series_obj
                                    .get("meta")
                                    .and_then(|meta| meta.get("square_cover"))
                                    .and_then(|v| v.as_str())
                            })
                            .or_else(|| {
                                series_obj
                                    .get("meta")
                                    .and_then(|meta| meta.get("horizontal_cover"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("")
                            .to_string();

                        current_page_collections.push(crate::api::response::UserCollection {
                            collection_type: "series".to_string(),
                            sid: series_obj
                                .get("meta")
                                .and_then(|meta| meta.get("series_id"))
                                .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                                .unwrap_or(0)
                                .to_string(),
                            name: series_obj
                                .get("meta")
                                .and_then(|meta| meta.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            cover,
                            description: series_obj
                                .get("meta")
                                .and_then(|meta| meta.get("description"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            total: series_obj
                                .get("meta")
                                .and_then(|meta| meta.get("total"))
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0),
                            ptime: series_obj
                                .get("meta")
                                .and_then(|meta| meta.get("mtime"))
                                .and_then(|v| v.as_i64()),
                            mid,
                        });
                    }
                }
            }

            // 如果当前页没有数据，说明已经获取完毕
            if current_page_collections.is_empty() {
                debug!("UP主 {} 第 {} 页没有合集数据，停止获取", mid, current_page);
                break;
            }

            // 添加当前页的合集到总列表
            all_collections.extend(current_page_collections);
            debug!(
                "UP主 {} 第 {} 页获取到 {} 个合集，累计 {} 个",
                mid,
                current_page,
                all_collections.len() - (current_page as usize - 1) * page_size as usize,
                all_collections.len()
            );

            // 检查是否已经获取了所有合集
            if total_count > 0 && all_collections.len() >= total_count as usize {
                debug!("UP主 {} 已获取所有 {} 个合集", mid, total_count);
                break;
            }

            // 如果当前页的合集数量少于page_size，说明是最后一页
            let current_page_size = all_collections.len() - (current_page as usize - 1) * page_size as usize;
            if current_page_size < page_size as usize {
                debug!(
                    "UP主 {} 第 {} 页合集数量 {} 少于页面大小 {}，已获取完毕",
                    mid, current_page, current_page_size, page_size
                );
                break;
            }

            current_page += 1;

            // 添加延迟以避免请求过于频繁
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }

        let collection_count = all_collections.len();
        info!("UP主 {} 总共获取到 {} 个合集", mid, collection_count);

        crate::utils::collection_aggregate::cache_user_collections(mid, &all_collections);

        Ok(crate::api::response::UserCollectionsResponse {
            success: true,
            collections: all_collections,
            total: total_count,
            page: 1,                            // 返回固定值，因为我们返回的是所有合集
            page_size: collection_count as u32, // 返回实际获取的合集数量
        })
    }

    /// 获取关注的UP主列表
    pub async fn get_user_followings(&self) -> Result<Vec<UserFollowingInfo>, anyhow::Error> {
        let uid = self.get_current_user_id()?;

        let url = "https://api.bilibili.com/x/relation/followings";
        let mut all_followings = Vec::new();
        let mut page = 1;
        let page_size = 50; // bilibili API限制最大为50

        loop {
            let response = self
                .request(Method::GET, url)
                .await
                .query(&[
                    ("vmid", &uid.to_string()),
                    ("pn", &page.to_string()),
                    ("ps", &page_size.to_string()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<serde_json::Value>()
                .await?
                .validate()?;

            let list = response["data"]["list"]
                .as_array()
                .ok_or_else(|| anyhow!("响应格式错误：缺少list字段"))?;

            // 如果当前页没有数据，说明已经获取完毕
            if list.is_empty() {
                break;
            }

            let current_page_followings: Vec<UserFollowingInfo> = list
                .iter()
                .filter_map(|item| {
                    let mid = item["mid"].as_i64()?;
                    let name = item["uname"].as_str()?.to_string();
                    let face = item["face"].as_str()?.to_string();
                    let sign = item["sign"].as_str().unwrap_or("").to_string();

                    let official_verify = item["official_verify"].as_object().map(|verify| UserOfficialVerify {
                        type_: verify["type"].as_i64().unwrap_or(-1) as i32,
                        desc: verify["desc"].as_str().unwrap_or("").to_string(),
                    });

                    Some(UserFollowingInfo {
                        mid,
                        name,
                        face,
                        sign,
                        official_verify,
                        follower: None, // 关注列表API不返回粉丝数
                    })
                })
                .collect();

            all_followings.extend(current_page_followings);

            // 如果当前页数据不足50个，说明是最后一页
            if list.len() < page_size as usize {
                break;
            }

            page += 1;

            // 添加延迟以避免请求过于频繁
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        Ok(all_followings)
    }

    /// 获取UP主个人简介（签名）
    pub async fn get_user_sign(&self, up_id: i64) -> Result<Option<String>, anyhow::Error> {
        use crate::bilibili::Validate;

        let url = "https://api.bilibili.com/x/web-interface/card";
        let response = self
            .public_request(Method::GET, url)
            .await
            .query(&[("mid", up_id.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;

        let sign = response["data"]["card"]["sign"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if sign.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sign))
        }
    }

    /// 获取用户关注的合集和收藏夹列表
    pub async fn get_subscribed_collections(
        &self,
    ) -> Result<Vec<crate::api::response::UserCollectionInfo>, anyhow::Error> {
        let uid = self.get_current_user_id()?;

        let url = "https://api.bilibili.com/x/v3/fav/folder/collected/list";
        let mut all_collections = Vec::new();
        let mut page = 1;
        let page_size = 20;

        loop {
            let response = self
                .request(Method::GET, url)
                .await
                .query(&[
                    ("up_mid", &uid.to_string()),
                    ("pn", &page.to_string()),
                    ("ps", &page_size.to_string()),
                    ("platform", &"web".to_string()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<serde_json::Value>()
                .await?
                .validate()?;

            let list = response["data"]["list"].as_array();
            if list.is_none() || list.unwrap().is_empty() {
                break;
            }

            let list = list.unwrap();

            for item in list {
                if let Some(item_obj) = item.as_object() {
                    // 根据 type 字段区分：type=11 是收藏夹，type=21 是合集
                    let item_type = item_obj["type"].as_i64().unwrap_or(0);
                    let collection_type = if item_type == 11 {
                        "favorite" // 收藏夹
                    } else {
                        "season" // 合集 (type=21)
                    };

                    all_collections.push(crate::api::response::UserCollectionInfo {
                        sid: item_obj["id"].as_i64().unwrap_or(0).to_string(),
                        name: item_obj["title"].as_str().unwrap_or("").to_string(),
                        cover: item_obj.get("cover").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        description: item_obj.get("intro").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        total: item_obj["media_count"].as_i64().unwrap_or(0) as i32,
                        collection_type: collection_type.to_string(),
                        up_name: item_obj["upper"]["name"].as_str().unwrap_or("").to_string(),
                        up_mid: item_obj["upper"]["mid"].as_i64().unwrap_or(0),
                    });
                }
            }

            if list.len() < page_size {
                break;
            }

            page += 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        if all_collections.is_empty() {
            info!("当前用户暂无关注的合集/收藏夹");
        } else {
            let favorites_count = all_collections
                .iter()
                .filter(|c| c.collection_type == "favorite")
                .count();
            let seasons_count = all_collections.iter().filter(|c| c.collection_type == "season").count();
            info!(
                "获取到用户关注的 {} 个合集和 {} 个收藏夹",
                seasons_count, favorites_count
            );
        }

        Ok(all_collections)
    }

    /// 获取UP主投稿视频列表 - 用于选择性下载历史投稿
    pub async fn get_user_submission_videos(
        &self,
        up_id: i64,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<crate::api::response::SubmissionVideoInfo>, i64), anyhow::Error> {
        self.get_user_submission_videos_with_keyword(up_id, page, page_size, None)
            .await
    }

    /// 搜索UP主投稿视频 - 支持关键词搜索
    pub async fn search_user_submission_videos(
        &self,
        up_id: i64,
        keyword: &str,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<crate::api::response::SubmissionVideoInfo>, i64), anyhow::Error> {
        self.get_user_submission_videos_with_keyword(up_id, page, page_size, Some(keyword))
            .await
    }

    /// 获取UP主投稿视频列表的内部实现 - 支持可选关键词搜索
    async fn get_user_submission_videos_with_keyword(
        &self,
        up_id: i64,
        page: i32,
        page_size: i32,
        keyword: Option<&str>,
    ) -> Result<(Vec<crate::api::response::SubmissionVideoInfo>, i64), anyhow::Error> {
        use crate::bilibili::Validate;

        let url = "https://api.bilibili.com/x/space/wbi/arc/search";

        // 获取wbi签名参数
        let wbi_img = self.wbi_img().await?;
        let mut params = std::collections::HashMap::new();
        params.insert("mid".to_string(), up_id.to_string());
        params.insert("pn".to_string(), page.to_string());
        params.insert("ps".to_string(), page_size.to_string());
        params.insert("order".to_string(), "pubdate".to_string()); // 按发布时间排序
        params.insert("order_avoided".to_string(), "true".to_string());

        // 如果提供了关键词，添加到搜索参数中
        if let Some(keyword) = keyword {
            params.insert("keyword".to_string(), keyword.to_string());
        }

        // 对参数进行wbi签名
        let signed_params = wbi_img.sign_params(params).await?;

        let response = self
            .request(Method::GET, url)
            .await
            .query(&signed_params)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;

        // 解析视频列表
        let video_list = response["data"]["list"]["vlist"]
            .as_array()
            .ok_or_else(|| anyhow!("无法获取投稿视频列表"))?;

        // 获取总数
        let total = response["data"]["page"]["count"].as_i64().unwrap_or(0);

        let mut videos = Vec::new();
        for video_item in video_list {
            // 解析发布时间
            let pubtime_timestamp = video_item["created"].as_i64().unwrap_or(0);
            let pubtime = crate::utils::time_format::timestamp_to_standard_string(pubtime_timestamp);

            let video_info = crate::api::response::SubmissionVideoInfo {
                bvid: video_item["bvid"].as_str().unwrap_or("").to_string(),
                title: video_item["title"].as_str().unwrap_or("").to_string(),
                cover: video_item["pic"].as_str().unwrap_or("").to_string(),
                pubtime,
                duration: video_item["length"]
                    .as_str()
                    .and_then(|s| {
                        // 将 "mm:ss" 格式转换为秒数
                        let parts: Vec<&str> = s.split(':').collect();
                        match parts.len() {
                            2 => {
                                let minutes = parts[0].parse::<i32>().unwrap_or(0);
                                let seconds = parts[1].parse::<i32>().unwrap_or(0);
                                Some(minutes * 60 + seconds)
                            }
                            3 => {
                                let hours = parts[0].parse::<i32>().unwrap_or(0);
                                let minutes = parts[1].parse::<i32>().unwrap_or(0);
                                let seconds = parts[2].parse::<i32>().unwrap_or(0);
                                Some(hours * 3600 + minutes * 60 + seconds)
                            }
                            _ => None,
                        }
                    })
                    .unwrap_or(0),
                view: video_item["play"].as_i64().unwrap_or(0) as i32,
                danmaku: video_item["video_review"].as_i64().unwrap_or(0) as i32,
                description: video_item["description"].as_str().unwrap_or("").to_string(),
            };

            videos.push(video_info);
        }

        Ok((videos, total))
    }

    /// 设置gaia_vtoken缓存
    pub fn set_gaia_vtoken(&self, token: String) {
        self.gaia_vtoken.store(Some(Arc::new(token)));
        tracing::info!("已缓存gaia_vtoken");
    }

    /// 获取缓存的gaia_vtoken
    pub fn get_gaia_vtoken(&self) -> Option<String> {
        self.gaia_vtoken.load().as_ref().map(|token| (**token).clone())
    }

    /// 获取csrf token (bili_jct)
    pub fn get_csrf_token(&self) -> Option<String> {
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        credential.as_ref().map(|cred| cred.bili_jct.clone())
    }
}
