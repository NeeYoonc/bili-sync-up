//! B 站课程（pugv / cheese）接口封装。
//!
//! 课程和普通稿件、番剧都不是同一套接口：
//! - 详情：`https://api.bilibili.com/pugv/view/web/season?season_id=...`
//! - 取流：`https://api.bilibili.com/pugv/player/web/playurl`（见 `Video::get_pugv_page_analyzer_*`）
//!
//! 课程 URL 形如 <https://www.bilibili.com/cheese/play/ss713799843>，
//! 其中的 `713799843` 即 `season_id`。

use std::pin::Pin;

use anyhow::{bail, Result};
use async_stream::try_stream;
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::Stream;
use reqwest::Method;
use tracing::{debug, info, warn};

use super::{BiliClient, VideoInfo};

pub struct Pugv {
    client: BiliClient,
    season_id: Option<String>,
    ep_id: Option<String>,
}

/// 讲师（UP 主）名下的一门课程
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PugvUpCourse {
    pub season_id: String,
    pub title: String,
    pub cover: Option<String>,
    pub episode_count: Option<i64>,
    /// 课程列表里的进度文案，例如「已更新479课时」
    pub status: Option<String>,
    pub subtitle: Option<String>,
}

impl Pugv {
    pub fn new(client: &BiliClient, season_id: Option<String>, ep_id: Option<String>) -> Self {
        Self {
            client: client.clone(),
            season_id,
            ep_id,
        }
    }

    /// 获取指定讲师（UP 主）名下的全部课程。
    ///
    /// 对应空间页「课程」标签使用的接口 `pugv/app/web/season/page`，
    /// 未登录也能取到公开课程列表。
    pub async fn fetch_up_courses(client: &BiliClient, up_mid: &str) -> Result<Vec<PugvUpCourse>> {
        const PAGE_SIZE: u32 = 30;
        /// 防御性上限，避免上游 `next` 字段异常时无限翻页
        const MAX_PAGES: u32 = 20;

        let mut courses = Vec::new();
        for page in 1..=MAX_PAGES {
            let page_string = page.to_string();
            let size_string = PAGE_SIZE.to_string();
            let json: serde_json::Value = client
                .request(Method::GET, "https://api.bilibili.com/pugv/app/web/season/page")
                .await
                .query(&[
                    ("mid", up_mid),
                    ("pn", page_string.as_str()),
                    ("ps", size_string.as_str()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            let data = validate(json)?;
            if let Some(items) = data["items"].as_array() {
                for item in items {
                    let season_id = match item["season_id"].as_i64() {
                        Some(season_id) if season_id > 0 => season_id.to_string(),
                        _ => match item["season_id"].as_str() {
                            Some(season_id) if !season_id.is_empty() => season_id.to_string(),
                            _ => continue,
                        },
                    };
                    courses.push(PugvUpCourse {
                        season_id,
                        title: item["title"].as_str().unwrap_or_default().to_string(),
                        cover: item["cover"].as_str().map(|s| s.to_string()),
                        episode_count: item["ep_count"].as_i64(),
                        status: item["status"].as_str().map(|s| s.to_string()),
                        subtitle: item["subtitle"].as_str().map(|s| s.to_string()),
                    });
                }
            }

            if !data["page"]["next"].as_bool().unwrap_or(false) {
                break;
            }
        }

        debug!("讲师 {} 名下共获取到 {} 门课程", up_mid, courses.len());
        Ok(courses)
    }

    /// 通过 season_id 获取课程详情。
    ///
    /// 这里使用带凭证的请求，因为响应中的 `user_status` / `payment` 与当前账号的
    /// 购买状态相关（未登录时 `payed` 恒为 0，不能作为判断依据）。
    pub async fn get_season_info(&self) -> Result<serde_json::Value> {
        let season_id = match (&self.season_id, &self.ep_id) {
            (Some(season_id), _) => season_id.clone(),
            (None, Some(ep_id)) => {
                let url = format!("https://api.bilibili.com/pugv/view/web/season?ep_id={}", ep_id);
                let json: serde_json::Value = self
                    .client
                    .request(Method::GET, url.as_str())
                    .await
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                let data = validate(json)?;
                data["season_id"]
                    .as_i64()
                    .map(|v| v.to_string())
                    .or_else(|| data["season_id"].as_str().map(ToOwned::to_owned))
                    .unwrap_or_default()
            }
            (None, None) => bail!("课程源缺少 season_id 和 ep_id"),
        };

        if season_id.is_empty() {
            bail!("无法确定课程的 season_id");
        }

        let url = format!("https://api.bilibili.com/pugv/view/web/season?season_id={}", season_id);
        let json: serde_json::Value = self
            .client
            .request(Method::GET, url.as_str())
            .await
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        validate(json)
    }

    /// 打印当前账号在该课程上的购买状态，便于排查「为什么下不了」。
    ///
    /// 注意：0 元课（「0元抢学」「免费试听」这类）压根没有购买记录，
    /// `user_status.payed` 恒为 0，不能据此判定「未购买」并告警——
    /// 这类课程的课时本来就能直接取流。
    pub async fn log_purchase_status(&self) {
        match self.get_season_info().await {
            Ok(data) => {
                let title = data["title"].as_str().unwrap_or("未知");
                let payed = data["user_status"]["payed"].as_i64();
                let price = course_price_display(&data);

                match classify_course_purchase(&data) {
                    CoursePurchaseState::Purchased => info!(
                        "课程「{}」当前账号已购买（payed={:?}, 有效期: {}）",
                        title,
                        payed,
                        data["user_status"]["user_expiry_content"]
                            .as_str()
                            .unwrap_or("长期有效")
                    ),
                    CoursePurchaseState::Free => info!(
                        "课程「{}」为免费课程（价格 {}），无需购买，全部课时可直接取流",
                        title, price
                    ),
                    CoursePurchaseState::Unpurchased | CoursePurchaseState::Unknown => warn!(
                        "课程「{}」当前账号未检测到购买记录（payed={:?}, 是否过期: {:?}, 价格: {}），付费课时将无法取流；若确认已购买，请检查 B 站凭证是否有效",
                        title,
                        payed,
                        data["user_status"]["is_expired"].as_bool(),
                        price
                    ),
                }
            }
            Err(e) => warn!("获取课程购买状态失败: {:#}", e),
        }
    }

    /// 生成课程课时流。
    ///
    /// 与番剧不同，课程里「尚未开播的直播 / 回放生成中」的课时 `release_date == 0`，
    /// 且 aid/cid 都是同一组占位值，这类课时直接跳过；其余课时都会带真实发布时间，
    /// 因此增量仍然用发布时间过滤。
    pub fn to_video_stream_incremental(
        &self,
        latest_row_at: Option<NaiveDateTime>,
    ) -> Pin<Box<dyn Stream<Item = Result<VideoInfo>> + Send>> {
        let client = self.client.clone();
        let season_id = self.season_id.clone();
        let ep_id = self.ep_id.clone();

        Box::pin(try_stream! {
            let pugv = Pugv::new(&client, season_id, ep_id);
            let season_info = pugv.get_season_info().await?;

            let season_id_value = season_info["season_id"]
                .as_i64()
                .map(|v| v.to_string())
                .or_else(|| season_info["season_id"].as_str().map(ToOwned::to_owned))
                .unwrap_or_default();
            if season_id_value.is_empty() {
                Err(anyhow::anyhow!("课程详情缺少 season_id"))?;
            }

            let course_title = season_info["title"].as_str().unwrap_or_default().to_string();
            let course_cover = season_info["cover"].as_str().unwrap_or_default().to_string();
            let intro = {
                let content = season_info["brief"]["content"].as_str().unwrap_or_default().trim();
                if content.is_empty() {
                    season_info["subtitle"].as_str().unwrap_or_default().to_string()
                } else {
                    content.to_string()
                }
            };
            let lecturer = season_info["up_info"]["uname"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| season_info["cooperators"].as_array().and_then(|v| v.first()).and_then(|v| v["uname"].as_str()).map(ToOwned::to_owned));
            let lecturer_id = season_info["up_info"]["mid"].as_i64();
            let lecturer_face = season_info["up_info"]["avatar"].as_str().map(|s| s.to_string());

            let episodes = season_info["episodes"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("课程详情缺少 episodes 字段"))?
                .clone();

            info!(
                "课程「{}」共 {} 个课时，开始解析",
                course_title,
                episodes.len()
            );

            let mut emitted = 0usize;
            let mut skipped_unpublished = 0usize;
            let mut skipped_by_time = 0usize;

            for episode in &episodes {
                let status = episode["status"].as_i64().unwrap_or(0);
                let episode_title = episode["title"].as_str().unwrap_or_default().to_string();
                let release_ts = episode["release_date"].as_i64().unwrap_or_default();
                let play_way = episode["play_way"].as_i64().unwrap_or_default();

                // `status` 只区分「试看」与「正式」课时（本课程 489 节里只有 3 节是试看，
                // status=1），并不是上架标记，因此不能用它过滤。
                // 真正未就绪的课时（尚未开播的直播、回放生成中）特征是 `release_date == 0`，
                // 并且它们的 aid/cid 是同一组占位值，必须跳过。
                if release_ts <= 0 {
                    skipped_unpublished += 1;
                    debug!(
                        "跳过未发布课时：{} (status={}, play_way={}, subtitle={:?})",
                        episode_title,
                        status,
                        play_way,
                        episode["subtitle"].as_str().unwrap_or("")
                    );
                    continue;
                }

                let ep_id_value = episode["id"].as_i64().unwrap_or_default();
                if ep_id_value == 0 {
                    warn!("课时「{}」缺少 ep id，跳过", episode_title);
                    continue;
                }

                let aid = episode["aid"].as_i64().unwrap_or_default();
                let cid = episode["cid"].as_i64().unwrap_or_default();
                if aid == 0 || cid == 0 {
                    warn!("课时「{}」缺少 aid/cid，跳过", episode_title);
                    continue;
                }

                let pubtime = DateTime::<Utc>::from_timestamp(release_ts, 0).unwrap_or_else(Utc::now);

                // 增量过滤：只取发布时间晚于上次记录的课时
                if let Some(latest_time) = latest_row_at {
                    let pubtime_beijing = pubtime
                        .with_timezone(&crate::utils::time_format::beijing_timezone())
                        .naive_local();
                    if pubtime_beijing <= latest_time {
                        skipped_by_time += 1;
                        continue;
                    }
                }

                let duration_secs = episode["duration"].as_i64().unwrap_or_default();
                let episode_cover = episode["cover"].as_str().unwrap_or(&course_cover).to_string();
                let episode_number = episode["index"]
                    .as_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .filter(|v| *v > 0);

                let share_copy = Some(if course_title.is_empty() {
                    episode_title.clone()
                } else {
                    format!("{} {}", course_title, episode_title)
                });

                emitted += 1;
                yield VideoInfo::Pugv {
                    title: course_title.clone(),
                    season_id: season_id_value.clone(),
                    ep_id: ep_id_value.to_string(),
                    bvid: format!("av{}", aid),
                    aid: aid.to_string(),
                    cid: cid.to_string(),
                    cover: episode_cover,
                    intro: intro.clone(),
                    pubtime,
                    duration: (duration_secs > 0).then_some(duration_secs as i32),
                    show_title: Some(episode_title),
                    episode_number,
                    share_copy,
                    lecturer: lecturer.clone(),
                    lecturer_id,
                    lecturer_face: lecturer_face.clone(),
                };
            }

            info!(
                "课程「{}」解析完成：本次产出 {} 个课时，跳过未发布 {} 个，跳过旧课时 {} 个",
                course_title, emitted, skipped_unpublished, skipped_by_time
            );
        })
    }
}

/// 课程购买状态（只影响日志提示，不参与任何下载逻辑）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoursePurchaseState {
    /// 当前账号已购买
    Purchased,
    /// 免费 / 0 元课，无需购买（这类课程 payed 恒为 0）
    Free,
    /// 付费课，但当前账号查不到购买记录
    Unpurchased,
    /// 接口没给价格，无法判断免费还是付费，保守按未购买提示
    Unknown,
}

/// 判断课程购买状态。
///
/// 注意顺序：先看 `payed`，再看价格。免费课（0 元）永远不会产生购买记录，
/// 只看 `payed` 会把「0元抢学」「免费试听」这类课程误报成「未购买」。
fn classify_course_purchase(season_info: &serde_json::Value) -> CoursePurchaseState {
    let payed = season_info["user_status"]["payed"].as_i64().unwrap_or(0);
    if payed > 0 {
        return CoursePurchaseState::Purchased;
    }
    match course_price(season_info) {
        Some(price) if price <= 0.0 => CoursePurchaseState::Free,
        Some(_) => CoursePurchaseState::Unpurchased,
        None => CoursePurchaseState::Unknown,
    }
}

/// 读取课程价格：优先 `payment.price`（数值），缺失时回退解析 `payment.price_format`。
///
/// 返回 `None` 表示响应里没有可用的价格字段（接口结构变化等），
/// 此时**不能**断言课程免费，避免把付费课误判成免费课。
fn course_price(season_info: &serde_json::Value) -> Option<f64> {
    if let Some(price) = season_info["payment"]["price"].as_f64() {
        return Some(price);
    }
    let raw = season_info["payment"]["price_format"].as_str()?;
    let cleaned: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    cleaned.parse::<f64>().ok()
}

/// 日志里展示用的价格文本：优先用接口给的 `price_format`（如 "88"、"0"），否则退回数值。
fn course_price_display(season_info: &serde_json::Value) -> String {
    if let Some(raw) = season_info["payment"]["price_format"].as_str() {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    match course_price(season_info) {
        Some(price) => price.to_string(),
        None => "-".to_string(),
    }
}

/// 校验 `{code, message, data}` 响应并返回 `data`
fn validate(json: serde_json::Value) -> Result<serde_json::Value> {
    let (code, message) = match (json["code"].as_i64(), json["message"].as_str()) {
        (Some(code), Some(message)) => (code, message),
        _ => bail!("课程接口返回内容缺少 code/message"),
    };
    if code != 0 {
        return Err(crate::bilibili::BiliError::RequestFailed(code, message.to_string()).into());
    }
    Ok(json["data"].clone())
}
#[cfg(test)]
mod tests {
    use super::{classify_course_purchase, course_price, course_price_display, CoursePurchaseState};
    use serde_json::json;

    /// 真实响应（2026-10-03 抓取）：free 课 payment.price = 0.0 / price_format = "0"，
    /// 且 user_status.payed 同样是 0 —— 这正是之前被误报「未检测到购买记录」的原因。
    fn free_course_payload() -> serde_json::Value {
        json!({
            "title": "【0元抢学】颉斌斌 || 考研英语28考研早鸟班",
            "payment": { "price": 0.0, "price_format": "0", "refresh_text": "免费报名" },
            "user_status": { "payed": 0, "is_expired": false, "user_expiry_content": "长期有效" }
        })
    }

    /// 真实响应：付费课 payment.price = 188.0，未购买账号 payed = 0
    fn paid_course_payload() -> serde_json::Value {
        json!({
            "title": "颉斌斌 || 【188元】27考研英语全程班",
            "payment": { "price": 188.0, "price_format": "188" },
            "user_status": { "payed": 0, "is_expired": false, "user_expiry_content": "长期有效" }
        })
    }

    #[test]
    fn free_course_is_not_reported_as_unpurchased() {
        let data = free_course_payload();
        assert_eq!(classify_course_purchase(&data), CoursePurchaseState::Free);
        assert_eq!(course_price(&data), Some(0.0));
        assert_eq!(course_price_display(&data), "0");
    }

    #[test]
    fn paid_course_without_purchase_still_warns() {
        let data = paid_course_payload();
        assert_eq!(classify_course_purchase(&data), CoursePurchaseState::Unpurchased);
        assert_eq!(course_price_display(&data), "188");
    }

    #[test]
    fn purchased_course_wins_over_price() {
        let mut data = paid_course_payload();
        data["user_status"]["payed"] = json!(1);
        assert_eq!(classify_course_purchase(&data), CoursePurchaseState::Purchased);
    }

    #[test]
    fn price_falls_back_to_price_format() {
        let data = json!({ "payment": { "price_format": "¥ 88" } });
        assert_eq!(course_price(&data), Some(88.0));
        assert_eq!(classify_course_purchase(&data), CoursePurchaseState::Unpurchased);
    }

    #[test]
    fn missing_payment_is_unknown_not_free() {
        let data = json!({ "title": "x" });
        assert_eq!(course_price(&data), None);
        assert_eq!(classify_course_purchase(&data), CoursePurchaseState::Unknown);
        assert_eq!(course_price_display(&data), "-");
    }
}
