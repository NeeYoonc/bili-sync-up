use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use futures::Stream;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::bilibili::credential::encoded_query;
use crate::bilibili::{BiliClient, Validate, VideoInfo, MIXIN_KEY};

#[derive(PartialEq, Eq, Hash, Clone, Debug)]
pub enum CollectionType {
    Series,
    Season,
}

impl From<CollectionType> for i32 {
    fn from(v: CollectionType) -> Self {
        match v {
            CollectionType::Series => 1,
            CollectionType::Season => 2,
        }
    }
}

impl From<i32> for CollectionType {
    fn from(v: i32) -> Self {
        match v {
            1 => CollectionType::Series,
            2 => CollectionType::Season,
            _ => panic!("invalid collection type"),
        }
    }
}

impl Display for CollectionType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CollectionType::Series => "列表",
            CollectionType::Season => "合集",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CollectionItem {
    pub mid: String,
    pub sid: String,
    pub collection_type: CollectionType,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum CollectionEpisodeOrderStrategy {
    Legacy = 0,
    SeasonHeadTailOldestFirst = 1,
}

impl From<CollectionEpisodeOrderStrategy> for i32 {
    fn from(v: CollectionEpisodeOrderStrategy) -> Self {
        match v {
            CollectionEpisodeOrderStrategy::Legacy => 0,
            CollectionEpisodeOrderStrategy::SeasonHeadTailOldestFirst => 1,
        }
    }
}

impl From<i32> for CollectionEpisodeOrderStrategy {
    fn from(v: i32) -> Self {
        match v {
            1 => CollectionEpisodeOrderStrategy::SeasonHeadTailOldestFirst,
            _ => CollectionEpisodeOrderStrategy::Legacy,
        }
    }
}

pub struct Collection<'a> {
    client: &'a BiliClient,
    collection: &'a CollectionItem,
}

#[derive(Debug, PartialEq)]
pub struct CollectionInfo {
    pub name: String,
    pub mid: i64,
    pub sid: i64,
    pub collection_type: CollectionType,
}

impl<'de> Deserialize<'de> for CollectionInfo {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct CollectionInfoRaw {
            mid: i64,
            name: String,
            season_id: Option<i64>,
            series_id: Option<i64>,
        }
        let raw = CollectionInfoRaw::deserialize(deserializer)?;
        let (sid, collection_type) = match (raw.season_id, raw.series_id) {
            (Some(sid), None) => (sid, CollectionType::Season),
            (None, Some(sid)) => (sid, CollectionType::Series),
            _ => return Err(serde::de::Error::custom("invalid collection info")),
        };
        Ok(CollectionInfo {
            mid: raw.mid,
            name: raw.name,
            sid,
            collection_type,
        })
    }
}

impl<'a> Collection<'a> {
    pub fn new(client: &'a BiliClient, collection: &'a CollectionItem) -> Self {
        Self { client, collection }
    }

    pub async fn get_cover_url(&self) -> Result<String> {
        let meta = match self.collection.collection_type {
            CollectionType::Season => self.get_videos(1).await?["data"]["meta"].clone(),
            CollectionType::Series => self.get_series_info().await?["data"]["meta"].clone(),
        };

        meta.get("cover")
            .and_then(|v| v.as_str())
            .or_else(|| meta.get("square_cover").and_then(|v| v.as_str()))
            .or_else(|| meta.get("horizontal_cover").and_then(|v| v.as_str()))
            .filter(|cover| !cover.trim().is_empty())
            .map(|cover| cover.to_string())
            .ok_or_else(|| anyhow!("合集封面URL为空"))
    }

    pub async fn get_info(&self) -> Result<CollectionInfo> {
        let meta = match self.collection.collection_type {
            // 没有找到专门获取 Season 信息的接口，所以直接获取第一页，从里面取 meta 信息
            CollectionType::Season => self.get_videos(1).await?["data"]["meta"].take(),
            CollectionType::Series => self.get_series_info().await?["data"]["meta"].take(),
        };
        Ok(serde_json::from_value(meta)?)
    }

    async fn get_series_info(&self) -> Result<Value> {
        self.client
            .request(Method::GET, "https://api.bilibili.com/x/series/series")
            .await
            .query(&[("series_id", self.collection.sid.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?
            .validate()
    }

    async fn get_videos(&self, page: i32) -> Result<Value> {
        let page = page.to_string();
        let (url, query) = match self.collection.collection_type {
            CollectionType::Series => (
                "https://api.bilibili.com/x/series/archives",
                encoded_query(
                    vec![
                        ("mid", self.collection.mid.as_str()),
                        ("series_id", self.collection.sid.as_str()),
                        ("only_normal", "true"),
                        ("sort", "desc"),
                        ("pn", page.as_str()),
                        ("ps", "30"),
                    ],
                    MIXIN_KEY.load().as_deref(),
                ),
            ),
            CollectionType::Season => (
                "https://api.bilibili.com/x/polymer/web-space/seasons_archives_list",
                encoded_query(
                    vec![
                        ("mid", self.collection.mid.as_str()),
                        ("season_id", self.collection.sid.as_str()),
                        ("page_num", page.as_str()),
                        ("page_size", "30"),
                    ],
                    MIXIN_KEY.load().as_deref(),
                ),
            ),
        };
        self.client
            .request(Method::GET, url)
            .await
            .query(&query)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?
            .validate()
    }

    pub fn into_video_stream(self) -> impl Stream<Item = Result<VideoInfo>> + 'a {
        try_stream! {
            let mut page = 1;
            loop {
                let mut videos = self.get_videos(page).await.with_context(|| {
                    format!(
                        "failed to get videos of collection {:?} page {}",
                        self.collection, page
                    )
                })?;
                let archives = &mut videos["data"]["archives"];
                if archives.as_array().is_none_or(|v| v.is_empty()) {
                    Err(anyhow!(
                        "no videos found in collection {:?} page {}",
                        self.collection,
                        page
                    ))?;
                }
                let videos_info: Vec<VideoInfo> = serde_json::from_value(archives.take()).with_context(|| {
                    format!(
                        "failed to parse videos of collection {:?} page {}",
                        self.collection, page
                    )
                })?;
                for video_info in videos_info {
                    yield video_info;
                }
                let page_info = &videos["data"]["page"];
                let fields = match self.collection.collection_type {
                    CollectionType::Series => ["num", "size", "total"],
                    CollectionType::Season => ["page_num", "page_size", "total"],
                };
                let values = fields
                    .iter()
                    .map(|f| page_info[f].as_i64())
                    .collect::<Vec<Option<i64>>>();
                if let [Some(num), Some(size), Some(total)] = values[..] {
                    if num * size < total {
                        page += 1;
                        continue;
                    }
                } else {
                    Err(anyhow!(
                        "invalid page info of collection {:?} page {}: read {:?} from {}",
                        self.collection,
                        page,
                        fields,
                        page_info
                    ))?;
                }
                break;
            }
        }
    }

    /// 获取合集中所有视频的正确顺序。
    /// - Legacy：保持旧合集源行为。
    /// - SeasonHeadTailOldestFirst：对 season 合集取网页默认顺序后，只比较首尾投稿时间，
    ///   若首个视频更新于末尾视频，则整体反转，确保 E01 位于更旧的一端。
    /// 返回 bvid -> episode_number 的映射
    pub async fn get_video_order_map(&self, strategy: CollectionEpisodeOrderStrategy) -> Result<HashMap<String, i32>> {
        let mut order_map = HashMap::new();
        let mut ordered_videos = self.get_video_order_entries(strategy).await?;
        if self.collection.collection_type == CollectionType::Season
            && strategy == CollectionEpisodeOrderStrategy::SeasonHeadTailOldestFirst
        {
            if should_reverse_season_order(&ordered_videos) {
                ordered_videos.reverse();
                debug!(
                    "合集 {:?} 默认顺序首尾方向为从新到旧，已整体反转以确保最旧视频为 E01",
                    self.collection
                );
            } else {
                debug!(
                    "合集 {:?} 默认顺序首尾方向已满足最旧视频在前，保持网页默认顺序",
                    self.collection
                );
            }
        }

        for (episode_number, (bvid, _pubdate)) in ordered_videos.into_iter().enumerate() {
            order_map.insert(bvid, (episode_number as i32) + 1);
        }

        debug!(
            "获取合集 {:?} 的视频顺序，共 {} 个视频",
            self.collection,
            order_map.len()
        );
        Ok(order_map)
    }

    async fn get_video_order_entries(&self, strategy: CollectionEpisodeOrderStrategy) -> Result<Vec<(String, i64)>> {
        let mut ordered_videos = Vec::new();
        let mut page = 1;

        loop {
            let page_str = page.to_string();
            let (url, query) = match self.collection.collection_type {
                CollectionType::Series => (
                    "https://api.bilibili.com/x/series/archives",
                    encoded_query(
                        vec![
                            ("mid", self.collection.mid.as_str()),
                            ("series_id", self.collection.sid.as_str()),
                            ("only_normal", "true"),
                            ("sort", "asc"),
                            ("pn", page_str.as_str()),
                            ("ps", "30"),
                        ],
                        MIXIN_KEY.load().as_deref(),
                    ),
                ),
                CollectionType::Season => (
                    "https://api.bilibili.com/x/polymer/web-space/seasons_archives_list",
                    match strategy {
                        CollectionEpisodeOrderStrategy::Legacy => encoded_query(
                            vec![
                                ("mid", self.collection.mid.as_str()),
                                ("season_id", self.collection.sid.as_str()),
                                ("sort_reverse", "false"),
                                ("page_num", page_str.as_str()),
                                ("page_size", "30"),
                            ],
                            MIXIN_KEY.load().as_deref(),
                        ),
                        CollectionEpisodeOrderStrategy::SeasonHeadTailOldestFirst => encoded_query(
                            vec![
                                ("mid", self.collection.mid.as_str()),
                                ("season_id", self.collection.sid.as_str()),
                                ("page_num", page_str.as_str()),
                                ("page_size", "30"),
                            ],
                            MIXIN_KEY.load().as_deref(),
                        ),
                    },
                ),
            };

            let videos = self
                .client
                .request(Method::GET, url)
                .await
                .query(&query)
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await?
                .validate()?;

            let archives = &videos["data"]["archives"];
            if let Some(arr) = archives.as_array() {
                if arr.is_empty() {
                    break;
                }
                for video in arr {
                    if let Some(bvid) = video["bvid"].as_str() {
                        let pubdate = video["pubdate"].as_i64().unwrap_or_default();
                        ordered_videos.push((bvid.to_string(), pubdate));
                    }
                }
            } else {
                break;
            }

            let page_info = &videos["data"]["page"];
            let fields = match self.collection.collection_type {
                CollectionType::Series => ["num", "size", "total"],
                CollectionType::Season => ["page_num", "page_size", "total"],
            };
            let values: Vec<Option<i64>> = fields.iter().map(|f| page_info[f].as_i64()).collect();

            if let [Some(num), Some(size), Some(total)] = values[..] {
                if num * size >= total {
                    break;
                }
                page += 1;
            } else {
                break;
            }
        }

        Ok(ordered_videos)
    }
}

fn should_reverse_season_order(entries: &[(String, i64)]) -> bool {
    match (entries.first(), entries.last()) {
        (Some((_, first_pubdate)), Some((_, last_pubdate))) => first_pubdate > last_pubdate,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collection_info_parse() {
        let testcases = vec![
            (
                r#"
                    {
                        "category": 0,
                        "cover": "https://archive.biliimg.com/bfs/archive/a6fbf7a7b9f4af09d9cf40482268634df387ef68.jpg",
                        "description": "",
                        "mid": 521722088,
                        "name": "合集·【命运方舟全剧情解说】",
                        "ptime": 1714701600,
                        "season_id": 1987140,
                        "total": 10
                    }
                "#,
                CollectionInfo {
                    mid: 521722088,
                    name: "合集·【命运方舟全剧情解说】".to_owned(),
                    sid: 1987140,
                    collection_type: CollectionType::Season,
                },
            ),
            (
                r#"
                    {
                        "series_id": 387212,
                        "mid": 521722088,
                        "name": "提瓦特冒险记",
                        "description": "原神沙雕般的游戏体验",
                        "keywords": [
                            ""
                        ],
                        "creator": "",
                        "state": 2,
                        "last_update_ts": 1633167320,
                        "total": 3,
                        "ctime": 1633167320,
                        "mtime": 1633167320,
                        "raw_keywords": "",
                        "category": 1
                    }
                "#,
                CollectionInfo {
                    mid: 521722088,
                    name: "提瓦特冒险记".to_owned(),
                    sid: 387212,
                    collection_type: CollectionType::Series,
                },
            ),
        ];
        for (json, expect) in testcases {
            let info: CollectionInfo = serde_json::from_str(json).unwrap();
            assert_eq!(info, expect);
        }
    }

    #[test]
    fn test_should_reverse_season_order_oldest_first() {
        let entries = vec![
            ("BV1".to_string(), 100),
            ("BV2".to_string(), 200),
            ("BV3".to_string(), 300),
        ];
        assert!(!should_reverse_season_order(&entries));
    }

    #[test]
    fn test_should_reverse_season_order_newest_first() {
        let entries = vec![
            ("BV1".to_string(), 300),
            ("BV2".to_string(), 200),
            ("BV3".to_string(), 100),
        ];
        assert!(should_reverse_season_order(&entries));
    }

    #[test]
    fn test_should_reverse_season_order_mixed_but_first_is_newer() {
        let entries = vec![
            ("BV1".to_string(), 300),
            ("BV2".to_string(), 100),
            ("BV3".to_string(), 200),
        ];
        assert!(should_reverse_season_order(&entries));
    }

    #[test]
    fn test_should_reverse_season_order_mixed_but_first_is_older() {
        let entries = vec![
            ("BV1".to_string(), 100),
            ("BV2".to_string(), 300),
            ("BV3".to_string(), 200),
        ];
        assert!(!should_reverse_season_order(&entries));
    }

    #[test]
    fn test_collection_episode_order_strategy_from_i32() {
        assert_eq!(
            CollectionEpisodeOrderStrategy::from(0),
            CollectionEpisodeOrderStrategy::Legacy
        );
        assert_eq!(
            CollectionEpisodeOrderStrategy::from(1),
            CollectionEpisodeOrderStrategy::SeasonHeadTailOldestFirst
        );
        assert_eq!(
            CollectionEpisodeOrderStrategy::from(99),
            CollectionEpisodeOrderStrategy::Legacy
        );
    }
}
