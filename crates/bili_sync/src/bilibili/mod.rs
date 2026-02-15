use std::sync::Arc;

pub use analyzer::{AudioQuality, BestStream, FilterOption, Stream, VideoCodecs, VideoQuality};
use anyhow::{bail, ensure, Result};
use arc_swap::ArcSwapOption;
pub use captcha_server::{get_captcha_info, serve_captcha_page, submit_captcha_result};
pub use captcha_solver::CaptchaSolver;
use chrono::serde::ts_seconds;
use chrono::{DateTime, Utc};
pub use client::{BiliClient, Client, SearchResult};
pub use collection::{Collection, CollectionItem, CollectionType};
pub use credential::Credential;
pub use danmaku::DanmakuOption;
pub use dynamic::Dynamic;
pub use error::BiliError;
pub use favorite_list::FavoriteList;
use favorite_list::Upper;
use once_cell::sync::Lazy;
pub use risk_control::{CaptchaInfo, CaptchaResult, GeetestInfo, RiskControl};
pub use submission::Submission;
pub use verification_coordinator::{VerificationRequest, VERIFICATION_COORDINATOR};
pub use video::{bvid_to_aid, Dimension, PageInfo, Video};
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
        attr: i32,
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
        /// UP主信息，从arc.author中提取
        #[serde(rename = "arc")]
        arc: Option<serde_json::Value>,
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
