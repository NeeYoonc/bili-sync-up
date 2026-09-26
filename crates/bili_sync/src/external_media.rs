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
    /// 图集作品的段列表（图片段与视频段按作品内顺序混排，抖音 live photo /
    /// 视频混排都用它）；未填写的调用方仍可用 `images`，由
    /// `slideshow_segments()` 自动降级成纯图片段。
    #[serde(default)]
    pub(crate) slides: Vec<ExternalImageSlide>,
    /// 图文作品的配乐备选地址；普通视频为空。
    #[serde(default)]
    pub(crate) music_urls: Vec<String>,
    /// YouTube 联合投稿/合作创作频道名列表（yt-dlp `creators`），第一位通常是主频道；
    /// 非联合投稿或 yt-dlp 未返回该字段时为空。
    #[serde(default)]
    pub(crate) creators: Option<Vec<String>>,
}

/// 图集作品里的一个「段」：抖音图集可以混排图片与视频片段
/// （live photo、视频混排），TikTok 图片贴目前只有图片段。
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ExternalImageSlide {
    /// 图片段的原图备选地址；视频段这里放该段封面（用不到时为 0 个）。
    #[serde(default)]
    pub(crate) image_urls: Vec<String>,
    /// 视频片段的 MP4 备选地址（按码率从高到低）；图片段为空。
    #[serde(default)]
    pub(crate) video_urls: Vec<String>,
    /// 视频片段时长（秒）；图片段为空。
    #[serde(default)]
    pub(crate) duration: Option<f64>,
}

impl ExternalImageSlide {
    /// 该段是否至少有一种可取到的媒体地址。
    pub(crate) fn has_media(&self) -> bool {
        !self.image_urls.is_empty() || !self.video_urls.is_empty()
    }

    /// 该段是否为视频片段（live photo / 视频混排）。
    pub(crate) fn is_video(&self) -> bool {
        !self.video_urls.is_empty()
    }
}

impl ExternalMediaMetadata {
    /// 作品是否为图集/幻灯片（抖音图文、TikTok 图片贴）。
    pub(crate) fn is_slideshow(&self) -> bool {
        !self.slides.is_empty() || !self.images.is_empty()
    }

    /// 图集作品的段列表；调用方只填了 `images` 时降级成纯图片段。
    pub(crate) fn slideshow_segments(&self) -> Vec<ExternalImageSlide> {
        if !self.slides.is_empty() {
            return self.slides.clone();
        }
        self.images
            .iter()
            .cloned()
            .map(|image_urls| ExternalImageSlide {
                image_urls,
                video_urls: Vec::new(),
                duration: None,
            })
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_from_json(value: serde_json::Value) -> ExternalMediaMetadata {
        serde_json::from_value(value).expect("媒体元数据应能反序列化")
    }

    /// 只填了 `images` 的调用方（TikTok 图片贴、yt-dlp 图文兜底）必须继续按
    /// 纯图片段处理，不能因为新增 `slides` 字段就丢掉这些作品。
    #[test]
    fn slideshow_segments_falls_back_to_images() {
        let metadata = metadata_from_json(serde_json::json!({
            "id": "1",
            "images": [["https://example.com/1.jpg"], ["https://example.com/2.jpg"]],
        }));
        assert!(metadata.is_slideshow());
        let segments = metadata.slideshow_segments();
        assert_eq!(segments.len(), 2);
        assert!(segments.iter().all(|segment| !segment.is_video()));
        assert_eq!(segments[1].image_urls, vec!["https://example.com/2.jpg".to_string()]);
    }

    /// 纯视频图集（每段都是 live photo）`images` 为空，也必须被当成幻灯片，
    /// 否则会被误判成普通视频。
    #[test]
    fn slideshow_is_recognized_without_any_image() {
        let metadata = metadata_from_json(serde_json::json!({
            "id": "1",
            "slides": [{
                "video_urls": ["https://example.com/1.mp4"],
                "duration": 2.5
            }],
        }));
        assert!(metadata.images.is_empty());
        assert!(metadata.is_slideshow());
        assert!(metadata.slideshow_segments()[0].is_video());
    }

    /// 普通视频既没有原图也没有段，不应被当成幻灯片。
    #[test]
    fn normal_video_is_not_a_slideshow() {
        let metadata = metadata_from_json(serde_json::json!({ "id": "1" }));
        assert!(!metadata.is_slideshow());
        assert!(metadata.slideshow_segments().is_empty());
    }
}
