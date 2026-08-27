use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use futures::{Stream, StreamExt};
use serde_json::Value;
use tracing::{debug, warn};

use crate::bilibili::{BiliClient, Collection, CollectionItem, CollectionType, Validate, VideoInfo};
pub struct FavoriteList<'a> {
    client: &'a BiliClient,
    fid: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct FavoriteListInfo {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct Upper<T> {
    pub mid: T,
    pub name: String,
    pub face: String,
}
impl<'a> FavoriteList<'a> {
    pub fn new(client: &'a BiliClient, fid: String) -> Self {
        Self { client, fid }
    }

    pub async fn get_info(&self) -> Result<FavoriteListInfo> {
        let mut res = self
            .client
            .request(reqwest::Method::GET, "https://api.bilibili.com/x/v3/fav/folder/info")
            .await
            .query(&[("media_id", &self.fid)])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;
        Ok(serde_json::from_value(res["data"].take())?)
    }

    async fn get_videos(&self, page: u32) -> Result<Value> {
        self.client
            .request(reqwest::Method::GET, "https://api.bilibili.com/x/v3/fav/resource/list")
            .await
            .query(&[
                ("media_id", self.fid.as_str()),
                ("pn", page.to_string().as_str()),
                ("ps", "20"),
                ("order", "mtime"),
                ("type", "0"),
                ("tid", "0"),
                // 必须带 platform=web，否则收藏夹内收藏的「视频合集」(type=21) 会被B站接口过滤，
                // 导致这类收藏夹返回空 medias、无法扫描。
                ("platform", "web"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()
    }

    // 拿到收藏夹的所有权，返回一个收藏夹下的视频流
    //
    // 注意：收藏夹内可能收藏了「视频合集」（type=21），此时条目本身没有 bvid，
    // 只有合集 id 和 UP主 mid。这里会将合集展开为其中的每一集（VideoInfo::Collection），
    // 这样合集后续新增分集时才能在收藏夹扫描中被发现并下载。
    pub fn into_video_stream(self) -> impl Stream<Item = Result<VideoInfo>> + 'a {
        try_stream! {
            let mut page = 1;
            loop {
                let mut videos = self
                    .get_videos(page)
                    .await
                    .with_context(|| format!("failed to get videos of favorite {} page {}", self.fid, page))?;

                let media_count = videos["data"]["info"]["media_count"].as_u64().unwrap_or(0);
                let medias = &mut videos["data"]["medias"];

                if medias.as_array().is_none_or(|v| v.is_empty()) {
                    if media_count > 0 {
                        // 统计显示有视频但medias为空，说明内容被B站API过滤
                        // 只记录警告，不抛出错误，正常结束扫描
                        warn!("收藏夹 {} 中的 {} 个视频被B站API过滤，无法通过API获取（可能是番剧、纪录片等特殊内容类型）", self.fid, media_count);
                        break;
                    } else {
                        // 正常的空页面情况
                        break;
                    }
                }
                let medias = medias.as_array_mut().context("medias is not an array")?;
                for media in medias.iter_mut() {
                    // 视频合集（type=21）：收藏夹收藏了整个合集，展开为合集内的每一集
                    if media["type"].as_i64() == Some(21) {
                        match expand_favorite_collection(self.client, media).await {
                            Ok(episodes) => {
                                for episode in episodes {
                                    yield episode;
                                }
                            }
                            Err(err) => {
                                warn!(
                                    "收藏夹 {} 中的视频合集展开失败，本轮跳过该合集: {:#}",
                                    self.fid, err
                                );
                            }
                        }
                        continue;
                    }
                    let video_info: VideoInfo = serde_json::from_value(media.take()).with_context(|| {
                        format!("failed to parse videos of favorite {} page {}", self.fid, page)
                    })?;
                    yield video_info;
                }
                let has_more = &videos["data"]["has_more"];
                if let Some(v) = has_more.as_bool() {
                    if v {
                        page += 1;
                        continue;
                    }
                } else {
                    Err(anyhow!("has_more is not a bool"))?;
                }
                break;
            }
        }
    }
}

/// 将收藏夹中的视频合集条目（type=21）展开为合集内的每一集。
async fn expand_favorite_collection(client: &BiliClient, media: &Value) -> Result<Vec<VideoInfo>> {
    let season_id = media["id"].as_i64().context("视频合集条目缺少 id")?;
    let upper_mid = media["upper"]["mid"].as_i64().context("视频合集条目缺少 upper.mid")?;
    // B站对已失效的合集返回 upper.mid=0，此时合集详情接口必然 404，
    // 直接按“已失效合集”处理，不需要再发起请求。
    if upper_mid <= 0 {
        let title = media["title"].as_str().unwrap_or("已失效合集");
        debug!("收藏夹中的合集已失效，跳过展开: title={}, season_id={}", title, season_id);
        return Ok(Vec::new());
    }
    let collection_item = CollectionItem {
        mid: upper_mid.to_string(),
        sid: season_id.to_string(),
        collection_type: CollectionType::Season,
    };
    let collection = Collection::new(client, &collection_item);
    let season_id_str = season_id.to_string();
    let mut episodes = Vec::new();
    let stream = collection.into_video_stream();
    futures::pin_mut!(stream);
    while let Some(episode) = stream.next().await {
        let mut episode = episode?;
        if let VideoInfo::Collection { season_id, arc, .. } = &mut episode {
            // 记录合集ID，便于后续增量扫描发现合集新增分集
            *season_id = Some(season_id_str.clone());
            // 合集分集接口返回的条目不带 author 字段，这里把合集作者 mid 补进去，
            // 保证入库时 upper_id 正确，后续合集分集巡检才能重建合集请求。
            if arc.is_none() {
                *arc = Some(serde_json::json!({}));
            }
            if let Some(arc_value) = arc.as_mut() {
                if arc_value["author"].is_null() {
                    arc_value["author"] = serde_json::json!({
                        "mid": upper_mid,
                        "name": media["upper"]["name"].as_str().unwrap_or(""),
                        "face": media["upper"]["face"].as_str().unwrap_or(""),
                    });
                }
            }
        }
        episodes.push(episode);
    }
    Ok(episodes)
}
