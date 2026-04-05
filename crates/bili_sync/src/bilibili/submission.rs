use anyhow::{anyhow, Context, Result};
use arc_swap::access::Access;
use async_stream::try_stream;
use futures::Stream;
use once_cell::sync::Lazy;
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::bilibili::credential::encoded_query;
use crate::bilibili::favorite_list::Upper;
use crate::bilibili::{BiliClient, Validate, VideoInfo, MIXIN_KEY};
use crate::config::SubmissionRiskControlConfig;
use crate::database::get_global_db;
use crate::utils::submission_checkpoint;

/// 全局提交源页码跟踪器，用于断点续传
/// 存储格式: (页码, 该页已处理的视频索引)
pub static SUBMISSION_PAGE_TRACKER: Lazy<RwLock<HashMap<String, (usize, usize)>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SubmissionAccountStatus {
    pub name: String,
    #[serde(default)]
    pub control: i32,
    #[serde(default)]
    pub silence: i32,
}

pub struct Submission<'a> {
    client: &'a BiliClient,
    upper_id: String,
    upper_name: Option<String>,
}

impl<'a> Submission<'a> {
    pub fn new(client: &'a BiliClient, upper_id: String) -> Self {
        Self {
            client,
            upper_id,
            upper_name: None,
        }
    }

    pub fn with_name(client: &'a BiliClient, upper_id: String, upper_name: String) -> Self {
        Self {
            client,
            upper_id,
            upper_name: Some(upper_name),
        }
    }

    fn display_name(&self) -> &str {
        self.upper_name.as_deref().unwrap_or(&self.upper_id)
    }

    pub async fn get_info(&self) -> Result<Upper<String>> {
        let mut res = self
            .client
            .public_request(Method::GET, "https://api.bilibili.com/x/web-interface/card")
            .await
            .query(&[("mid", self.upper_id.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;
        Ok(serde_json::from_value(res["data"]["card"].take())?)
    }

    pub async fn get_account_status(&self) -> Result<SubmissionAccountStatus> {
        let mut res = self
            .client
            .request(Method::GET, "https://api.bilibili.com/x/space/wbi/acc/info")
            .await
            .query(&encoded_query(
                vec![
                    ("mid", self.upper_id.as_str()),
                    ("platform", "web"),
                    ("web_location", "1550101"),
                    ("token", ""),
                ],
                MIXIN_KEY.load().as_deref(),
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;
        Ok(serde_json::from_value(res["data"].take())?)
    }

    async fn get_videos(&self, page: i32) -> Result<Value> {
        self.client
            .request(Method::GET, "https://api.bilibili.com/x/space/wbi/arc/search")
            .await
            .query(&encoded_query(
                vec![
                    ("mid", self.upper_id.as_str()),
                    ("order", "pubdate"),
                    ("order_avoided", "true"),
                    ("platform", "web"),
                    ("web_location", "1550101"),
                    ("pn", page.to_string().as_str()),
                    ("ps", "30"),
                ],
                MIXIN_KEY.load().as_deref(),
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()
    }

    pub fn into_video_stream(
        self,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> impl Stream<Item = Result<VideoInfo>> + 'a {
        try_stream! {
            let _page: usize = 1;
            let mut request_count = 0;
            let mut total_video_count: Option<i64> = None;
            let mut is_large_submission = false;

            let current_config = crate::config::reload_config();
            let config = &current_config.submission_risk_control;

            // 获取上次中断的页码和视频索引
            let (mut page, skip_videos_count) = match self.get_last_processed_checkpoint().await {
                Ok((saved_page, video_index)) if saved_page > 1 || video_index > 0 => {
                    if video_index > 0 {
                        info!("UP主 {} 从断点页码 {} 第 {} 个视频后继续获取", self.display_name(), saved_page, video_index);
                        debug!("断点恢复详情: 页码={}, 跳过前{}个视频", saved_page, video_index);
                    } else {
                        info!("UP主 {} 从断点页码 {} 继续获取", self.display_name(), saved_page);
                        debug!("断点恢复详情: 页码={}, 无需跳过视频", saved_page);
                    }
                    (saved_page, video_index)
                },
                _ => {
                    debug!("无断点记录，从第1页开始获取");
                    (1, 0)
                }
            };

            // 记录恢复信息
            let _is_resuming_from_checkpoint = page > 1 || skip_videos_count > 0;
            let resume_page = page;  // 记录恢复的起始页码，用于判断是否需要跳过视频
            let mut current_skip_count = skip_videos_count;  // 当前页需要跳过的视频数

            loop {
                // 在每次循环开始时检查取消状态
                if cancellation_token.is_cancelled() {
                    info!("UP主 {} 获取过程被取消，当前页码: {}", self.display_name(), page);
                    // 保存当前页码用于断点续传（页面开始时取消，从当前页重新开始）
                    if let Err(e) = self.save_last_processed_checkpoint(page, 0).await {
                        warn!("保存断点失败: {}", e);
                    }
                    return;
                }
                // 在第一次请求后实施延迟策略
                if request_count > 0 {
                    let delay = Self::calculate_adaptive_delay(
                        request_count,
                        is_large_submission,
                        config,
                    );

                    if delay.as_millis() > 0 {
                        debug!(
                            "UP主 {} 第 {} 次请求前延迟 {}ms（大量视频UP主: {}）",
                            self.display_name(),
                            request_count + 1,
                            delay.as_millis(),
                            is_large_submission
                        );
                        tokio::time::sleep(delay).await;
                    }

                }

                // 再次检查取消状态（请求API前）
                if cancellation_token.is_cancelled() {
                    info!("UP主 {} 在第 {} 页请求前检测到取消信号", self.display_name(), page);
                    if let Err(e) = self.save_last_processed_checkpoint(page, 0).await {
                        warn!("保存断点失败: {}", e);
                    }
                    return;
                }

                let mut videos = self
                    .get_videos(page as i32)
                    .await
                    .map_err(|e| {
                        // 使用现有的错误分类系统检测风控
                        let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                        if classified_error.error_type == crate::error::ErrorType::RiskControl {
                            warn!(
                                "UP主 {} 第 {} 页获取触发风控: {}",
                                self.display_name(), page, classified_error.message
                            );
                            return crate::error::DownloadAbortError().into();
                        }

                        // 其他错误继续抛出
                        e.context(format!("failed to get videos of upper {} page {}", self.display_name(), page))
                    })?;

                request_count += 1;

                // 在第一次请求时检测是否为大量视频UP主并确定处理策略
                // 对于断点恢复，也需要重新检测UP主类型
                if request_count == 1 {
                    if let Some(count) = videos["data"]["page"]["count"].as_i64() {
                        total_video_count = Some(count);
                        is_large_submission = count > config.large_submission_threshold as i64;

                        if is_large_submission {
                            if config.enable_batch_processing {
                                info!(
                                    "检测到大量视频UP主 {} ({}个视频)，启用分批处理策略（批次大小：{}页，间隔：{}秒）",
                                    self.display_name(), count, config.batch_size, config.batch_delay_seconds
                                );
                            } else {
                                info!(
                                    "检测到大量视频UP主 {} ({}个视频)，启用保守请求策略",
                                    self.display_name(), count
                                );
                            }
                        } else {
                            debug!(
                                "UP主 {} 有 {} 个视频，使用标准请求策略",
                                self.display_name(), count
                            );
                        }
                    }
                }

                // 分批处理：每处理batch_size页后额外延迟
                // 在成功请求API后判断是否需要分批延迟（基于绝对页码位置）
                if is_large_submission && config.enable_batch_processing && page % config.batch_size == 0 {
                    let batch_delay = Duration::from_secs(config.batch_delay_seconds);
                    info!(
                        "UP主 {} 分批处理：完成第 {} 批（页码{}），延迟 {}秒",
                        self.display_name(),
                        page / config.batch_size,
                        page,
                        config.batch_delay_seconds
                    );
                    tokio::time::sleep(batch_delay).await;
                }

                let vlist = &mut videos["data"]["list"]["vlist"];
                if vlist.as_array().is_none_or(|v| v.is_empty()) {
                    if page == 1 {
                        Err(anyhow!("no medias found in upper {} page {}", self.display_name(), page))?;
                    } else {
                        // 非第一页没有视频表示已经到达末尾
                        break;
                    }
                }

                let videos_info: Vec<VideoInfo> = serde_json::from_value(vlist.take())
                    .with_context(|| format!("failed to parse videos of upper {} page {}", self.display_name(), page))?;

                debug!("第{}页获取到{}个视频，跳过前{}个", page, videos_info.len(), current_skip_count);
                for (video_index, video_info) in videos_info.into_iter().enumerate() {
                    // 如果是恢复的第一页，跳过已处理的视频
                    if page == resume_page && video_index < current_skip_count {
                        debug!("跳过已处理的视频: 第{}页第{}个（恢复模式）", page, video_index + 1);
                        continue;
                    }

                    // 在yield每个视频前检查取消状态
                    if cancellation_token.is_cancelled() {
                        info!("UP主 {} 在第 {} 页第 {} 个视频处理时检测到取消信号", self.display_name(), page, video_index + 1);
                        if let Err(e) = self.save_last_processed_checkpoint(page, video_index).await {
                            warn!("保存断点失败: {}", e);
                        }
                        return;
                    }
                    yield video_info;
                }

                // 每页处理完成后保存断点
                if let Err(e) = self.save_last_processed_checkpoint(page + 1, 0).await {
                    warn!("保存页面完成断点失败: {}", e);
                }

                let count = &videos["data"]["page"]["count"];
                if let Some(v) = count.as_i64() {
                    if v > (page * 30) as i64 {
                        debug!("切换到第 {} 页继续处理", page + 1);
                        page += 1;
                        // 进入新页面时，清零跳过计数
                        current_skip_count = 0;
                        continue;
                    }
                } else {
                    Err(anyhow!("count is not an i64"))?;
                }
                break;
            }

            // 注意：这里不清除断点记录，因为我们还没有扫描完成
            // 只有在整个流结束时才清除（在扫描完成后记录统计信息的部分）

            // 扫描完成后记录统计信息并清除断点记录
            if let Some(total_count) = total_video_count {
                // 扫描完成，清除断点记录
                if let Err(e) = self.clear_last_processed_checkpoint().await {
                    warn!("清除断点失败: {}", e);
                }
                if is_large_submission && config.enable_batch_processing {
                    let total_batches = (page - 1).div_ceil(config.batch_size);
                    info!(
                        "UP主 {} 分批扫描完成：共 {} 个视频，处理到第{}页，分 {} 批处理（每批{}页），发起 {} 次API请求",
                        self.display_name(), total_count, page - 1, total_batches, config.batch_size, request_count
                    );
                } else {
                    info!(
                        "UP主 {} 扫描完成：共 {} 个视频，发起 {} 次API请求",
                        self.display_name(), total_count, request_count
                    );
                }
            }
        }
    }

    /// 获取上次处理的检查点（用于断点续传）
    async fn get_last_processed_checkpoint(&self) -> anyhow::Result<(usize, usize)> {
        let tracker = SUBMISSION_PAGE_TRACKER.read().unwrap();
        let checkpoint = tracker.get(&self.upper_id).copied().unwrap_or((1, 0));
        Ok(checkpoint)
    }

    /// 保存当前处理的检查点
    async fn save_last_processed_checkpoint(&self, page: usize, video_index: usize) -> anyhow::Result<()> {
        // 保存到内存
        {
            let mut tracker = SUBMISSION_PAGE_TRACKER.write().unwrap();
            tracker.insert(self.upper_id.clone(), (page, video_index));
        }

        // 持久化到数据库
        if let Some(db) = get_global_db() {
            submission_checkpoint::save_checkpoints_to_db(&db).await?;
        }

        if video_index > 0 {
            debug!("保存UP主 {} 的断点: 第{}页第{}个视频", self.upper_id, page, video_index);
        } else {
            debug!("保存UP主 {} 的断点页码: {}", self.upper_id, page);
        }
        Ok(())
    }

    /// 清除保存的检查点（完整扫描完成后）
    async fn clear_last_processed_checkpoint(&self) -> anyhow::Result<()> {
        // 从内存清除
        {
            let mut tracker = SUBMISSION_PAGE_TRACKER.write().unwrap();
            if tracker.remove(&self.upper_id).is_some() {
                info!("清除UP主 {} 的断点（扫描完成）", self.upper_id);
            }
        }

        // 持久化到数据库（清除该UP主的断点）
        if let Some(db) = get_global_db() {
            submission_checkpoint::save_checkpoints_to_db(&db).await?;
        }

        Ok(())
    }

    /// 计算自适应延迟时间
    fn calculate_adaptive_delay(
        request_count: usize,
        is_large_submission: bool,
        config: &SubmissionRiskControlConfig,
    ) -> Duration {
        let base_delay = Duration::from_millis(config.base_request_delay);

        // 大量视频UP主使用额外倍数
        let large_submission_multiplier = if is_large_submission {
            config.large_submission_delay_multiplier
        } else {
            1
        };

        // 渐进式延迟：请求次数越多，延迟越长
        let progressive_multiplier = if config.enable_progressive_delay {
            (request_count as u64 / 5).min(config.max_delay_multiplier - 1) + 1
        } else {
            1
        };

        let total_multiplier = large_submission_multiplier * progressive_multiplier;
        base_delay * total_multiplier as u32
    }
}
