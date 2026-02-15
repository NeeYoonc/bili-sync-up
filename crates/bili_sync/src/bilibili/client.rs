use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwapOption;
use leaky_bucket::RateLimiter;
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

    pub async fn check_refresh(&self) -> Result<()> {
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        let Some(credential) = credential.as_deref() else {
            return Ok(());
        };
        if !credential.need_refresh(&self.client).await? {
            return Ok(());
        }
        let new_credential = credential.refresh(&self.client).await?;
        config.credential.store(Some(Arc::new(new_credential.clone())));

        // 将刷新后的credential通过任务队列保存到数据库
        if let Err(e) = self.enqueue_credential_save_task(new_credential).await {
            warn!("将credential刷新任务加入队列失败: {}", e);
        } else {
            info!("credential已刷新，保存任务已加入队列");
        }

        Ok(())
    }

    /// 将credential刷新任务加入配置任务队列
    async fn enqueue_credential_save_task(&self, new_credential: crate::bilibili::Credential) -> Result<()> {
        use uuid::Uuid;

        // 更新内存中的配置
        let updated_config = crate::config::reload_config();
        updated_config.credential.store(Some(Arc::new(new_credential)));

        // 创建重载配置任务，让任务队列处理数据库保存
        let reload_task = crate::task::ReloadConfigTask {
            task_id: Uuid::new_v4().to_string(),
        };

        // 将任务加入队列
        let db = Arc::new(crate::database::setup_database().await);
        crate::task::enqueue_reload_task(reload_task, &db).await?;

        Ok(())
    }

    /// 获取 wbi img，用于生成请求签名
    pub async fn wbi_img(&self) -> Result<WbiImg> {
        let config = crate::config::reload_config();
        let credential = config.credential.load();
        let credential = credential.as_deref().context("no credential found")?;
        credential.wbi_img(&self.client).await
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

        let response = self.request(Method::GET, url).await.query(&params).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("搜索请求失败: {}", response.status()));
        }

        let search_response: SearchResponse = response.json().await?;

        if search_response.code != 0 {
            return Err(anyhow!("搜索API返回错误: {}", search_response.message));
        }

        let data = search_response.data.unwrap_or(SearchData {
            result: None,
            num_pages: None,
            num_results: None,
        });

        let results = data.result.unwrap_or_default();

        // 获取总数和页数
        let total = data.num_results.unwrap_or(0) as u32;
        let num_pages = data.num_pages.unwrap_or(0) as u32;
        let num_pages = if num_pages == 0 {
            if total > 0 {
                total.div_ceil(page_size) // 向上取整
            } else {
                1
            }
        } else {
            num_pages
        };

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
        use serde_json::Value;

        // 同时获取合集(seasons)和系列(series)
        let mut all_collections = Vec::new();
        let seasons_url = "https://api.bilibili.com/x/polymer/web-space/seasons_series_list";

        // 循环获取所有页面的数据
        let mut current_page = 1u32;
        let mut total_count = 0u32;

        loop {
            let mut retry_count = 0;
            let max_retries = 2;

            // 添加重试机制获取当前页数据
            let seasons_response = loop {
                match self
                    .request(Method::GET, seasons_url)
                    .await
                    .query(&[
                        ("mid", &mid.to_string()),
                        ("page_num", &current_page.to_string()),
                        ("page_size", &page_size.to_string()),
                    ])
                    .send()
                    .await
                {
                    Ok(response) => match response.error_for_status() {
                        Ok(response) => match response.json::<Value>().await {
                            Ok(json) => match json.validate() {
                                Ok(validated) => break validated,
                                Err(e) => {
                                    warn!("UP主 {} 合集响应验证失败: {}", mid, e);
                                    if retry_count >= max_retries {
                                        return Err(e.context("合集响应验证失败"));
                                    }
                                }
                            },
                            Err(e) => {
                                warn!("UP主 {} 合集JSON解析失败: {}", mid, e);
                                if retry_count >= max_retries {
                                    return Err(anyhow!("解析合集响应JSON失败: {}", e));
                                }
                            }
                        },
                        Err(e) => {
                            warn!("UP主 {} 合集请求状态错误: {}", mid, e);
                            if retry_count >= max_retries {
                                return Err(anyhow!("合集请求返回错误状态: {}", e));
                            }
                        }
                    },
                    Err(e) => {
                        warn!(
                            "UP主 {} 合集请求失败 (重试 {}/{}): {}",
                            mid,
                            retry_count + 1,
                            max_retries + 1,
                            e
                        );
                        if retry_count >= max_retries {
                            return Err(anyhow!("发送合集请求失败: {}", e));
                        }
                    }
                }

                retry_count += 1;
                // 重试前等待
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
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
