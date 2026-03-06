use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use chrono::DateTime;
use futures::Stream;
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::bilibili::credential::encoded_query;
use crate::bilibili::{BiliClient, Validate, VideoInfo, MIXIN_KEY};
use crate::config::SubmissionRiskControlConfig;

pub struct Dynamic<'a> {
    client: &'a BiliClient,
    pub upper_id: String,
}

impl<'a> Dynamic<'a> {
    pub fn new(client: &'a BiliClient, upper_id: String) -> Self {
        Self { client, upper_id }
    }

    async fn get_dynamics(&self, offset: Option<&str>) -> Result<Value> {
        self.client
            .request(
                Method::GET,
                "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/space",
            )
            .await
            .query(&encoded_query(
                vec![
                    ("host_mid", self.upper_id.as_str()),
                    ("offset", offset.unwrap_or("")),
                    ("type", "video"),
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
        cancellation_token: CancellationToken,
    ) -> impl Stream<Item = Result<VideoInfo>> + 'a {
        try_stream! {
            let mut offset: Option<String> = None;
            let mut request_count = 0usize;
            let current_config = crate::config::reload_config();
            let risk_config = current_config.submission_risk_control;

            loop {
                if cancellation_token.is_cancelled() {
                    return;
                }

                if request_count > 0 && risk_config.enable_dynamic_api_delay {
                    let delay = Self::calculate_dynamic_api_delay(request_count, &risk_config);
                    if !delay.is_zero() {
                        if request_count % 10 == 0 {
                            info!(
                                "动态API请求节流：UP主={}，已请求={}次，当前延迟={}ms",
                                self.upper_id,
                                request_count,
                                delay.as_millis()
                            );
                        } else {
                            debug!(
                                "动态API请求延迟：UP主={}，第{}次请求前延迟={}ms",
                                self.upper_id,
                                request_count + 1,
                                delay.as_millis()
                            );
                        }
                        tokio::time::sleep(delay).await;
                    }
                }

                if cancellation_token.is_cancelled() {
                    return;
                }

                let mut res = self
                    .get_dynamics(offset.as_deref())
                    .await
                    .with_context(|| "failed to get dynamics")?;
                request_count += 1;
                let items = res["data"]["items"].as_array_mut().context("items not exist")?;

                for item in items.iter_mut() {
                    if cancellation_token.is_cancelled() {
                        return;
                    }
                    if item["type"].as_str().is_none_or(|t| t != "DYNAMIC_TYPE_AV") {
                        continue;
                    }
                    let pub_ts = item["modules"]["module_author"]["pub_ts"].take();
                    let pub_dt = pub_ts
                        .as_i64()
                        .or_else(|| pub_ts.as_str().and_then(|s| s.parse::<i64>().ok()))
                        .and_then(|secs| DateTime::from_timestamp(secs, 0))
                        .with_context(|| format!("invalid pub_ts: {:?}", pub_ts))?;
                    let mut video_info: VideoInfo =
                        serde_json::from_value(item["modules"]["module_dynamic"]["major"]["archive"].take())?;
                    if let VideoInfo::Dynamic { ref mut pubtime, .. } = video_info {
                        *pubtime = pub_dt;
                        yield video_info;
                    } else {
                        Err(anyhow!("video info is not dynamic"))?;
                    }
                }

                if let (Some(has_more), Some(new_offset)) =
                    (res["data"]["has_more"].as_bool(), res["data"]["offset"].as_str())
                {
                    if !has_more {
                        break;
                    }
                    offset = Some(new_offset.to_string());
                } else {
                    Err(anyhow!("no has_more or offset found"))?;
                }
            }
        }
    }

    fn calculate_dynamic_api_delay(request_count: usize, config: &SubmissionRiskControlConfig) -> Duration {
        let base_delay_ms = (config.base_request_delay as f64 * config.dynamic_api_delay_multiplier).round();
        let base_delay_ms = if base_delay_ms.is_finite() && base_delay_ms > 0.0 {
            base_delay_ms as u64
        } else {
            0
        };
        if base_delay_ms == 0 {
            return Duration::ZERO;
        }

        let max_multiplier = config.max_delay_multiplier.max(1);
        let progressive_multiplier = if config.enable_progressive_delay {
            ((request_count as u64 / 5) + 1).min(max_multiplier)
        } else {
            1
        };

        Duration::from_millis(base_delay_ms.saturating_mul(progressive_multiplier))
    }
}
