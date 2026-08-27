use std::sync::Arc;

pub use analyzer::{AudioQuality, BestStream, FilterOption, PageAnalyzer, Stream, VideoCodecs, VideoQuality};
use anyhow::{bail, ensure, Result};
use arc_swap::ArcSwapOption;
pub use captcha_server::{get_captcha_info, serve_captcha_page, submit_captcha_result};
pub use captcha_solver::CaptchaSolver;
use chrono::serde::ts_seconds;
use chrono::{DateTime, Utc};
pub use client::{BiliClient, Client, SearchResult};
pub use collection::{Collection, CollectionEpisodeOrderStrategy, CollectionItem, CollectionType};
pub use credential::Credential;
pub use danmaku::{parse_event_name, DanmakuElem, DanmakuOption, DanmakuWriter};
pub use dynamic::Dynamic;
pub use error::BiliError;
pub use favorite_list::FavoriteList;
use favorite_list::Upper;
use once_cell::sync::Lazy;
pub use risk_control::{CaptchaInfo, CaptchaResult, GeetestInfo, RiskControl};
use serde::{Deserialize, Deserializer};
pub use submission::Submission;
pub(crate) use subtitle::{SubtitleDownloadOptions, DEFAULT_AI_SUBTITLE_LANGUAGE};
pub use verification_coordinator::{VerificationRequest, VERIFICATION_COORDINATOR};
pub(crate) use video::effective_playurl_qn_range;
pub use video::{
    bvid_to_aid, with_playurl_rate_limit, Dimension, PageInfo, PlayurlRateLimitConfig, Video, VideoChapter,
};
pub use watch_later::WatchLater;
pub mod bangumi;

mod analyzer;
mod captcha_server;
mod captcha_solver;
mod client;
mod collection;
mod credential;
mod danmaku;
mod dynamic;
mod error;
mod favorite_list;
mod risk_control;
pub mod submission;
mod subtitle;
mod verification_coordinator;
mod video;
mod watch_later;

static MIXIN_KEY: Lazy<ArcSwapOption<String>> = Lazy::new(Default::default);

pub(crate) fn set_global_mixin_key(key: String) {
    MIXIN_KEY.store(Some(Arc::new(key)));
}

fn parse_duration_to_seconds(raw: &str) -> Option<i32> {
    let normalized = raw.trim();
    if normalized.is_empty() {
        return None;
    }

    if let Ok(seconds) = normalized.parse::<i32>() {
        return Some(seconds);
    }

    let parts: Vec<&str> = normalized.split(':').collect();
    match parts.as_slice() {
        [minutes, seconds] => Some(minutes.parse::<i32>().ok()? * 60 + seconds.parse::<i32>().ok()?),
        [hours, minutes, seconds] => {
            Some(hours.parse::<i32>().ok()? * 3600 + minutes.parse::<i32>().ok()? * 60 + seconds.parse::<i32>().ok()?)
        }
        _ => None,
    }
}

fn deserialize_optional_duration_seconds<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|raw| match raw {
        serde_json::Value::Null => None,
        serde_json::Value::Number(num) => num.as_i64().map(|v| v as i32),
        serde_json::Value::String(text) => parse_duration_to_seconds(&text),
        _ => None,
    }))
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct StaffInfo {
    pub mid: i64,
    pub title: String,
    pub name: String,
    pub face: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follower: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_style: Option<i32>,
    // 忽略其他字段，如vip、official等
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct UgcSeasonEpisodePage {
    #[serde(default)]
    pub num: Option<i32>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct UgcSeasonEpisode {
    #[serde(default)]
    pub bvid: Option<String>,
    #[serde(default)]
    pub page: Option<UgcSeasonEpisodePage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct UgcSeasonInfo {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub mid: Option<i64>,
    #[serde(default)]
    pub episodes: Vec<UgcSeasonEpisode>,
}

pub(crate) trait Validate {
    type Output;

    fn validate(self) -> Result<Self::Output>;
}

impl Validate for serde_json::Value {
    type Output = serde_json::Value;

    fn validate(self) -> Result<Self::Output> {
        let (code, msg) = match (self["code"].as_i64(), self["message"].as_str()) {
            (Some(code), Some(msg)) => (code, msg),
            _ => bail!("no code or message found"),
        };
        ensure!(code == 0, BiliError::RequestFailed(code, msg.to_owned()));
        Ok(self)
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
/// 注意此处的顺序是有要求的，因为对于 untagged 的 enum 来说，serde 会按照顺序匹配
/// > There is no explicit tag identifying which variant the data contains.
/// > Serde will try to match the data against each variant in order and the first one that deserializes successfully is the one returned.
pub enum VideoInfo {
    /// 从视频详情接口获取的视频信息
    Detail {
        title: String,
        bvid: String,
        #[serde(rename = "desc")]
        intro: String,
        #[serde(rename = "pic")]
        cover: String,
        #[serde(rename = "owner")]
        upper: Upper<i64>,
        #[serde(with = "ts_seconds")]
        ctime: DateTime<Utc>,
        #[serde(rename = "pubdate", with = "ts_seconds")]
        pubtime: DateTime<Utc>,
        #[serde(default, deserialize_with = "deserialize_optional_duration_seconds")]
        duration: Option<i32>,
        pages: Vec<PageInfo>,
        state: i32,
        show_title: Option<String>,
        #[serde(default)]
        staff: Option<Vec<StaffInfo>>,
        /// 充电专享视频标识
        #[serde(default)]
        is_upower_exclusive: Option<bool>,
        /// 用户是否有权限观看充电专享视频
        #[serde(default)]
        is_upower_play: Option<bool>,
        /// UGC 合集信息（投稿视频中的合集/系列）
        #[serde(default)]
        ugc_season: Option<UgcSeasonInfo>,
    },
    /// 从收藏夹接口获取的视频信息
    Favorite {
        title: String,
        #[serde(rename = "type")]
        vtype: i32,
        bvid: String,
        intro: String,
        cover: String,
        upper: Upper<i64>,
        #[serde(with = "ts_seconds")]
        ctime: DateTime<Utc>,
        #[serde(with = "ts_seconds")]
        fav_time: DateTime<Utc>,
        #[serde(with = "ts_seconds")]
        pubtime: DateTime<Utc>,
        #[serde(default, deserialize_with = "deserialize_optional_duration_seconds")]
        duration: Option<i32>,
        attr: i32,
        /// 收藏夹接口返回的当前分P数（用于检测已收藏多P视频是否新增分P）
        #[serde(default)]
        page: i32,
    },
    /// 从稍后再看接口获取的视频信息
    WatchLater {
        title: String,
        bvid: String,
        #[serde(rename = "desc")]
        intro: String,
        #[serde(rename = "pic")]
        cover: String,
        #[serde(rename = "owner")]
        upper: Upper<i64>,
        #[serde(with = "ts_seconds")]
        ctime: DateTime<Utc>,
        #[serde(rename = "add_at", with = "ts_seconds")]
        fav_time: DateTime<Utc>,
        #[serde(rename = "pubdate", with = "ts_seconds")]
        pubtime: DateTime<Utc>,
        #[serde(default, deserialize_with = "deserialize_optional_duration_seconds")]
        duration: Option<i32>,
        state: i32,
    },
    /// 从视频合集/视频列表接口获取的视频信息
    Collection {
        bvid: String,
        #[serde(rename = "pic")]
        cover: String,
        #[serde(with = "ts_seconds")]
        ctime: DateTime<Utc>,
        #[serde(rename = "pubdate", with = "ts_seconds")]
        pubtime: DateTime<Utc>,
        /// 视频标题
        title: String,
        #[serde(default, deserialize_with = "deserialize_optional_duration_seconds")]
        duration: Option<i32>,
        /// UP主信息，从arc.author中提取
        #[serde(rename = "arc")]
        arc: Option<serde_json::Value>,
        /// 所属视频合集ID（收藏夹展开合集时写入，用于后续增量发现新增分集）
        #[serde(default)]
        season_id: Option<String>,
    },
    // 从用户投稿接口获取的视频信息
    Submission {
        title: String,
        bvid: String,
        #[serde(rename = "description")]
        intro: String,
        #[serde(rename = "pic")]
        cover: String,
        #[serde(rename = "created", with = "ts_seconds")]
        ctime: DateTime<Utc>,
        #[serde(
            rename = "length",
            default,
            deserialize_with = "deserialize_optional_duration_seconds"
        )]
        duration: Option<i32>,
        /// 投稿列表接口中的合集/系列ID（存在时用于UP源合集识别）
        #[serde(default)]
        season_id: Option<serde_json::Value>,
    },
    // 从动态接口获取的视频信息
    Dynamic {
        title: String,
        bvid: String,
        #[serde(rename = "desc")]
        intro: String,
        #[serde(rename = "cover")]
        cover: String,
        #[serde(default)]
        pubtime: DateTime<Utc>,
        #[serde(default, deserialize_with = "deserialize_optional_duration_seconds")]
        duration: Option<i32>,
    },
    // 从番剧接口获取的视频信息
    Bangumi {
        title: String,
        season_id: String,
        ep_id: String,
        bvid: String,
        cid: String,
        cover: String,
        intro: String,
        #[serde(with = "ts_seconds")]
        pubtime: DateTime<Utc>,
        duration: Option<i32>,
        show_title: Option<String>,
        /// 季度编号，从seasons数组中的位置计算得出
        season_number: Option<i32>,
        /// 集数，直接从API的title字段获取
        episode_number: Option<i32>,
        /// 详细的分享标题，用于NFO智能title选择
        share_copy: Option<String>,
        /// 番剧季度类型，用于区分常规番剧(1)和番剧影视(2)
        show_season_type: Option<i32>,
        /// 演员信息字符串，从API获取
        actors: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::VideoInfo;
    use serde_json::json;

    /// B站 view 接口对部分特殊状态视频（数据异常/下架中）返回的 data 缺少 pages/state 字段，
    /// untagged 反序列化会依次尝试各变体，最终匹配到 Collection 而不是 Detail。
    /// 这曾导致 workflow.rs 中 `let VideoInfo::Detail { .. } = view else { unreachable!() }` panic。
    #[test]
    fn view_data_without_pages_matches_collection_not_detail() {
        let data = json!({
            "bvid": "BV1test",
            "title": "测试视频",
            "pic": "https://example.com/cover.jpg",
            "ctime": 1620000000,
            "pubdate": 1620000000
        });
        let info: VideoInfo = serde_json::from_value(data).expect("应能解析为某个变体");
        assert!(
            matches!(info, VideoInfo::Collection { .. }),
            "缺少 pages 的 view 数据应解析为 Collection，实际: {:?}",
            std::mem::discriminant(&info)
        );
    }

    /// 收藏夹接口返回的 medias 条目应能解析出当前分P数（page 字段），
    /// 这是检测已收藏多P视频新增分P的基础。
    #[test]
    fn favorite_item_parses_page_count() {
        let data = json!({
            "id": 123456,
            "type": 2,
            "title": "测试多P视频",
            "intro": "",
            "cover": "https://example.com/cover.jpg",
            "upper": { "mid": 1, "name": "UP主", "face": "" },
            "ctime": 1620000000,
            "fav_time": 1620000000,
            "pubtime": 1620000000,
            "duration": 100,
            "attr": 0,
            "bv_id": "BV1test",
            "bvid": "BV1test",
            "page": 16
        });
        let info: VideoInfo = serde_json::from_value(data).expect("收藏夹条目应能解析");
        match info {
            VideoInfo::Favorite { bvid, page, .. } => {
                assert_eq!(bvid, "BV1test");
                assert_eq!(page, 16);
            }
            other => panic!("应解析为 Favorite 变体，实际: {:?}", std::mem::discriminant(&other)),
        }
    }

    /// 收藏夹接口未返回 page 字段时按 0 处理（历史字段缺失的容错）
    #[test]
    fn favorite_item_defaults_page_count_to_zero() {
        let data = json!({
            "id": 123456,
            "type": 2,
            "title": "测试视频",
            "intro": "",
            "cover": "https://example.com/cover.jpg",
            "upper": { "mid": 1, "name": "UP主", "face": "" },
            "ctime": 1620000000,
            "fav_time": 1620000000,
            "pubtime": 1620000000,
            "attr": 0,
            "bvid": "BV1test"
        });
        let info: VideoInfo = serde_json::from_value(data).expect("收藏夹条目应能解析");
        match info {
            VideoInfo::Favorite { page, .. } => assert_eq!(page, 0),
            other => panic!("应解析为 Favorite 变体，实际: {:?}", std::mem::discriminant(&other)),
        }
    }

    /// 正常 view 数据应解析为 Detail（回归保护）
    #[test]
    fn normal_view_data_matches_detail() {
        let data = json!({
            "bvid": "BV1test",
            "title": "测试视频",
            "desc": "描述",
            "pic": "https://example.com/cover.jpg",
            "owner": { "mid": 1, "name": "UP主", "face": "https://example.com/face.jpg" },
            "ctime": 1620000000,
            "pubdate": 1620000000,
            "duration": 100,
            "pages": [ { "cid": 1, "page": 1, "from": "vupload", "part": "P1", "duration": 100 } ],
            "state": 0
        });
        match serde_json::from_value::<VideoInfo>(data) {
            Ok(info) => assert!(
                matches!(info, VideoInfo::Detail { .. }),
                "正常 view 数据解析变体不符"
            ),
            Err(e) => panic!("正常 view 数据解析失败: {}", e),
        }
    }
}
