//! YouTube、抖音等外部视频源共用的媒体描述结构。
//!
//! 这里仅保存统一下载链路需要的数据，不包含任何平台 API、Cookie 或解析逻辑。

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct ExternalMediaMetadata {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) uploader: Option<String>,
    pub(crate) uploader_url: Option<String>,
    pub(crate) channel: Option<String>,
    pub(crate) channel_id: Option<String>,
    pub(crate) channel_url: Option<String>,
    pub(crate) thumbnail: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) upload_date: Option<String>,
    pub(crate) duration: Option<f64>,
    #[serde(default)]
    pub(crate) formats: Vec<ExternalMediaFormat>,
    #[serde(default)]
    pub(crate) subtitles: HashMap<String, Vec<ExternalSubtitle>>,
    #[serde(default)]
    pub(crate) automatic_captions: HashMap<String, Vec<ExternalSubtitle>>,
    /// 图文作品的全部原图备选地址；普通视频为空。
    #[serde(default)]
    pub(crate) images: Vec<Vec<String>>,
    /// 图文作品的配乐备选地址；普通视频为空。
    #[serde(default)]
    pub(crate) music_urls: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ExternalMediaFormat {
    pub(crate) format_id: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) protocol: Option<String>,
    pub(crate) ext: Option<String>,
    pub(crate) vcodec: Option<String>,
    pub(crate) acodec: Option<String>,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) fps: Option<f64>,
    pub(crate) tbr: Option<f64>,
    pub(crate) vbr: Option<f64>,
    pub(crate) abr: Option<f64>,
    pub(crate) dynamic_range: Option<String>,
    /// CENC 媒体的 16 字节内容密钥（32 位十六进制）。仅在平台明确返回
    /// 可本地解包的密钥材料时设置，统一下载器本身不接触密钥。
    #[serde(default)]
    pub(crate) decryption_key: Option<String>,
    #[serde(default)]
    pub(crate) fallback_urls: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ExternalSubtitle {
    pub(crate) url: Option<String>,
    pub(crate) ext: Option<String>,
}
