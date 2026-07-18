use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::bilibili::error::BiliError;

pub struct PageAnalyzer {
    info: serde_json::Value,
}

#[derive(
    Debug,
    strum::FromRepr,
    strum::EnumString,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Clone,
    Copy,
    ToSchema,
)]
pub enum VideoQuality {
    Quality360p = 16,
    Quality480p = 32,
    Quality720p = 64,
    Quality1080p = 80,
    Quality1080pPLUS = 112,
    Quality1080p60 = 116,
    Quality4k = 120,
    QualityHdr = 125,
    QualityDolby = 126,
    Quality8k = 127,
}

impl std::fmt::Display for VideoQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VideoQuality::Quality360p => write!(f, "360P流畅"),
            VideoQuality::Quality480p => write!(f, "480P清晰"),
            VideoQuality::Quality720p => write!(f, "720P高清"),
            VideoQuality::Quality1080p => write!(f, "1080P高清"),
            VideoQuality::Quality1080pPLUS => write!(f, "1080P+高码率"),
            VideoQuality::Quality1080p60 => write!(f, "1080P 60fps"),
            VideoQuality::Quality4k => write!(f, "4K超高清"),
            VideoQuality::QualityHdr => write!(f, "HDR真彩"),
            VideoQuality::QualityDolby => write!(f, "杜比视界"),
            VideoQuality::Quality8k => write!(f, "8K超高清"),
        }
    }
}

#[derive(Debug, Clone, Copy, strum::FromRepr, strum::EnumString, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum AudioQuality {
    Quality64k = 30216,
    Quality132k = 30232,
    QualityDolby = 30250,
    QualityHiRES = 30251,
    QualityDolbyBangumi = 30255,
    Quality192k = 30280,
}

impl Ord for AudioQuality {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_sort_key().cmp(&other.as_sort_key())
    }
}

impl PartialOrd for AudioQuality {
    fn partial_cmp(&self, other: &AudioQuality) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl AudioQuality {
    pub fn as_sort_key(&self) -> isize {
        match self {
            // 这可以让 Dolby 和 Hi-RES 排在 192k 之后，且 Dolby 和 Hi-RES 之间的顺序不变
            Self::QualityHiRES | Self::QualityDolby | Self::QualityDolbyBangumi => (*self as isize) + 40,
            _ => *self as isize,
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(
    Debug,
    strum::EnumString,
    strum::Display,
    strum::AsRefStr,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    Clone,
    Copy,
    ToSchema,
)]
pub enum VideoCodecs {
    #[strum(serialize = "HEV")]
    HEV,
    #[strum(serialize = "AVC")]
    AVC,
    #[strum(serialize = "AV1")]
    AV1,
}

impl TryFrom<u64> for VideoCodecs {
    type Error = anyhow::Error;

    fn try_from(value: u64) -> std::result::Result<Self, Self::Error> {
        // https://socialsisteryi.github.io/bilibili-API-collect/docs/video/videostream_url.html#%E8%A7%86%E9%A2%91%E7%BC%96%E7%A0%81%E4%BB%A3%E7%A0%81
        match value {
            7 => Ok(Self::AVC),
            12 => Ok(Self::HEV),
            13 => Ok(Self::AV1),
            _ => bail!("invalid video codecs id: {}", value),
        }
    }
}

// 视频流的筛选偏好
#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct FilterOption {
    pub video_max_quality: VideoQuality,
    pub video_min_quality: VideoQuality,
    pub audio_max_quality: AudioQuality,
    pub audio_min_quality: AudioQuality,
    pub codecs: Vec<VideoCodecs>,
    pub no_dolby_video: bool,
    pub no_dolby_audio: bool,
    pub no_hdr: bool,
    pub no_hires: bool,
}

impl Default for FilterOption {
    fn default() -> Self {
        Self {
            video_max_quality: VideoQuality::Quality8k,
            video_min_quality: VideoQuality::Quality360p,
            audio_max_quality: AudioQuality::QualityHiRES,
            audio_min_quality: AudioQuality::Quality64k,
            codecs: vec![VideoCodecs::AVC, VideoCodecs::HEV, VideoCodecs::AV1],
            no_dolby_video: false,
            no_dolby_audio: false,
            no_hdr: false,
            no_hires: false,
        }
    }
}

// 上游项目中的五种流类型，不过目测应该只有 Flv、DashVideo、DashAudio 三种会被用到
#[derive(Debug, PartialEq, PartialOrd)]
pub enum Stream {
    Flv(String),
    Html5Mp4(String),
    EpisodeTryMp4(String),
    DashVideo {
        url: String,
        backup_url: Vec<String>,
        quality: VideoQuality,
        codecs: VideoCodecs,
    },
    DashAudio {
        url: String,
        backup_url: Vec<String>,
        quality: AudioQuality,
    },
}

// 通用的获取流链接的方法，交由 Downloader 使用
impl Stream {
    pub fn urls(&self) -> Vec<&str> {
        match self {
            Self::Flv(url) | Self::Html5Mp4(url) | Self::EpisodeTryMp4(url) => vec![url],
            Self::DashVideo { url, backup_url, .. } | Self::DashAudio { url, backup_url, .. } => {
                let mut urls = std::iter::once(url.as_str())
                    .chain(backup_url.iter().map(|s| s.as_str()))
                    .collect();
                let current_config = crate::config::reload_config();
                if !current_config.cdn_sorting {
                    urls
                } else {
                    urls.sort_by_key(|u| {
                        if u.contains("upos-") {
                            0 // 服务商 cdn
                        } else if u.contains("cn-") {
                            1 // 自建 cdn
                        } else if u.contains("mcdn") {
                            2 // mcdn
                        } else {
                            3 // pcdn 或者其它
                        }
                    });
                    urls
                }
            }
        }
    }
}

/// 用于获取视频流的最佳筛选结果，有两种可能：
/// 1. 单个混合流，作为 Mixed 返回
/// 2. 视频、音频分离，作为 VideoAudio 返回，其中音频流可能不存在（对于无声视频，如 BV1J7411H7KQ）
#[derive(Debug)]
pub enum BestStream {
    VideoAudio { video: Stream, audio: Option<Stream> },
    Mixed(Stream),
}

impl PageAnalyzer {
    pub fn new(info: serde_json::Value) -> Self {
        Self { info }
    }

    fn is_flv_stream(&self) -> bool {
        self.info.get("durl").is_some() && self.info["format"].as_str().is_some_and(|f| f.contains("flv"))
    }

    fn is_html5_mp4_stream(&self) -> bool {
        self.info.get("durl").is_some()
            && self.info["format"].as_str().is_some_and(|f| f.contains("mp4"))
            && self.info["is_html5"].as_bool().is_some_and(|b| b)
    }

    fn is_episode_try_mp4_stream(&self) -> bool {
        self.info.get("durl").is_some()
            && self.info["format"].as_str().is_some_and(|f| f.contains("mp4"))
            && self.info["is_html5"].as_bool().is_none_or(|b| !b)
    }

    /// 获取所有的视频、音频流，并根据条件筛选
    fn streams(&mut self, filter_option: &FilterOption) -> Result<Vec<Stream>> {
        if self.is_flv_stream() {
            return Ok(vec![Stream::Flv(
                self.info["durl"][0]["url"]
                    .as_str()
                    .context("invalid flv stream")?
                    .to_string(),
            )]);
        }
        if self.is_html5_mp4_stream() {
            return Ok(vec![Stream::Html5Mp4(
                self.info["durl"][0]["url"]
                    .as_str()
                    .context("invalid html5 mp4 stream")?
                    .to_string(),
            )]);
        }
        if self.is_episode_try_mp4_stream() {
            return Ok(vec![Stream::EpisodeTryMp4(
                self.info["durl"][0]["url"]
                    .as_str()
                    .context("invalid episode try mp4 stream")?
                    .to_string(),
            )]);
        }

        // 优化的B站API响应调试日志
        tracing::debug!("=== B站视频流分析 ===");
        if let Some(video_array) = self.info.pointer("/dash/video").and_then(|v| v.as_array()) {
            tracing::debug!("API返回视频流数量: {}", video_array.len());
            let mut available_qualities = Vec::new();
            for video in video_array.iter() {
                if let Some(quality_id) = video["id"].as_u64() {
                    if let Some(quality) = VideoQuality::from_repr(quality_id as usize) {
                        available_qualities.push(format!("{:?}({})", quality, quality_id));
                    }
                }
            }
            if !available_qualities.is_empty() {
                tracing::debug!("可用质量: [{}]", available_qualities.join(", "));
            } else {
                tracing::warn!("未找到有效的视频质量信息");
            }
        } else {
            tracing::warn!("API响应中未找到dash/video数组");
        }

        tracing::debug!(
            "筛选条件: {}({}) - {}({}), 编码: {:?}",
            format!("{:?}", filter_option.video_max_quality),
            filter_option.video_max_quality as u32,
            format!("{:?}", filter_option.video_min_quality),
            filter_option.video_min_quality as u32,
            filter_option.codecs
        );
        tracing::debug!("=== 开始筛选 ===");

        let mut streams: Vec<Stream> = Vec::new();
        let mut filtered_count = 0;
        for (idx, video) in self
            .info
            .pointer_mut("/dash/video")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| BiliError::VideoStreamEmpty("API响应中未找到dash/video数组".to_string()))?
            .iter_mut()
            .enumerate()
        {
            let (Some(url), Some(quality_id), Some(codecs_id)) = (
                video["base_url"].as_str(),
                video["id"].as_u64(),
                video["codecid"].as_u64(),
            ) else {
                tracing::debug!("流 {}: 跳过 - 缺少必要字段", idx + 1);
                continue;
            };

            let quality = match VideoQuality::from_repr(quality_id as usize) {
                Some(q) => q,
                None => {
                    tracing::debug!("流 {}: 跳过 - 无效的质量ID {}", idx + 1, quality_id);
                    continue;
                }
            };

            let codecs = match codecs_id.try_into() {
                Ok(c) => c,
                Err(_) => {
                    tracing::debug!("流 {}: 跳过 - 无效的编码ID {}", idx + 1, codecs_id);
                    continue;
                }
            };

            // 筛选条件检查
            let mut passed = true;
            let mut filter_reason = String::new();

            if !filter_option.codecs.contains(&codecs) {
                passed = false;
                filter_reason = format!("编码{:?}不符", codecs);
            } else if quality < filter_option.video_min_quality {
                passed = false;
                filter_reason = format!("质量过低(<{:?})", filter_option.video_min_quality);
            } else if quality > filter_option.video_max_quality {
                passed = false;
                filter_reason = format!("质量过高(>{:?})", filter_option.video_max_quality);
            } else if quality == VideoQuality::QualityHdr && filter_option.no_hdr {
                passed = false;
                filter_reason = "HDR被禁用".to_string();
            } else if quality == VideoQuality::QualityDolby && filter_option.no_dolby_video {
                passed = false;
                filter_reason = "杜比视界被禁用".to_string();
            }

            if !passed {
                filtered_count += 1;
                tracing::debug!("过滤: {:?}({}) - {}", quality, quality_id, filter_reason);
                continue;
            }

            tracing::debug!("✓ 接受: {:?}({}) {:?}", quality, quality_id, codecs);

            streams.push(Stream::DashVideo {
                url: url.to_string(),
                backup_url: serde_json::from_value(video["backup_url"].take()).unwrap_or_default(),
                quality,
                codecs,
            });
        }

        let video_stream_count = streams.iter().filter(|s| matches!(s, Stream::DashVideo { .. })).count();
        tracing::debug!(
            "=== 筛选结果: {}个通过, {}个过滤 ===",
            video_stream_count,
            filtered_count
        );

        if video_stream_count == 0 {
            // 分析筛选失败的原因
            let max_quality_requested = filter_option.video_max_quality as u32;
            if max_quality_requested >= 120 {
                tracing::error!("❌ 无可用视频流！请求4K+质量({})可能需要：", max_quality_requested);
                tracing::error!("   1. 大会员权限");
                tracing::error!("   2. 视频本身支持该质量");
                tracing::error!("   3. 正确的登录状态");
                tracing::error!("   建议：降低质量要求或检查会员状态");
            } else {
                tracing::error!("❌ 无可用视频流！可能原因：");
                tracing::error!("   1. 网络问题或API访问失败");
                tracing::error!("   2. 视频不存在或已删除");
                tracing::error!("   3. 编码格式不匹配");
                tracing::error!("   建议：检查网络连接和编码设置");
            }
        }
        // 标记高品质音频是否异常
        let mut hires_failed = false;
        let mut dolby_failed = false;

        if let Some(audios) = self.info.pointer_mut("/dash/audio").and_then(|a| a.as_array_mut()) {
            tracing::debug!("发现音频流数组，共{}个音频流", audios.len());
            for (index, audio) in audios.iter_mut().enumerate() {
                let (Some(url), Some(quality)) = (audio["base_url"].as_str(), audio["id"].as_u64()) else {
                    tracing::warn!("音频流{}缺少base_url或id字段", index);
                    continue;
                };
                tracing::debug!(
                    "处理音频流{} - ID: {}, URL前缀: {}",
                    index,
                    quality,
                    &url[..url.len().min(50)]
                );
                let quality = AudioQuality::from_repr(quality as usize)
                    .context(format!("invalid audio stream quality: {}", quality))?;
                if quality < filter_option.audio_min_quality || quality > filter_option.audio_max_quality {
                    continue;
                }
                streams.push(Stream::DashAudio {
                    url: url.to_string(),
                    backup_url: serde_json::from_value(audio["backup_url"].take()).unwrap_or_default(),
                    quality,
                });
            }
        }
        if !filter_option.no_hires {
            if let Some(flac) = self.info.pointer_mut("/dash/flac/audio") {
                tracing::debug!("发现FLAC音频流");
                if let (Some(url), Some(quality)) = (flac["base_url"].as_str(), flac["id"].as_u64()) {
                    tracing::debug!(
                        "处理FLAC音频流 - ID: {}, URL前缀: {}",
                        quality,
                        &url[..url.len().min(50)]
                    );
                    let quality = AudioQuality::from_repr(quality as usize)
                        .context(format!("invalid flac stream quality: {}", quality))?;
                    if quality >= filter_option.audio_min_quality && quality <= filter_option.audio_max_quality {
                        streams.push(Stream::DashAudio {
                            url: url.to_string(),
                            backup_url: serde_json::from_value(flac["backup_url"].take()).unwrap_or_default(),
                            quality,
                        });
                    }
                } else {
                    tracing::warn!("FLAC音频流缺少base_url或id字段");
                    hires_failed = true;
                }
            }
        }
        if !filter_option.no_dolby_audio {
            // 首先检查dolby音频数组是否存在且非空
            if let Some(dolby_audio_array) = self
                .info
                .pointer_mut("/dash/dolby/audio")
                .and_then(|a| a.as_array_mut())
            {
                if !dolby_audio_array.is_empty() {
                    // 只有当dolby音频数组非空时才尝试处理
                    tracing::debug!("发现dolby音频流，数量: {}", dolby_audio_array.len());
                    // 记录完整的dolby音频数组信息
                    for (i, dolby_item) in dolby_audio_array.iter().enumerate() {
                        if let Some(id) = dolby_item.get("id").and_then(|v| v.as_u64()) {
                            tracing::debug!("Dolby音频流{} - ID: {}", i, id);
                        }
                    }
                    if let Some(dolby_audio) = dolby_audio_array.get_mut(0).and_then(|a| a.as_object_mut()) {
                        if let (Some(url), Some(quality)) = (
                            dolby_audio.get("base_url").and_then(|v| v.as_str()),
                            dolby_audio.get("id").and_then(|v| v.as_u64()),
                        ) {
                            tracing::debug!(
                                "处理Dolby音频流 - ID: {}, URL前缀: {}",
                                quality,
                                &url[..url.len().min(50)]
                            );
                            let quality = AudioQuality::from_repr(quality as usize)
                                .context(format!("invalid dolby audio stream quality: {}", quality))?;
                            if quality >= filter_option.audio_min_quality && quality <= filter_option.audio_max_quality
                            {
                                streams.push(Stream::DashAudio {
                                    url: url.to_string(),
                                    backup_url: serde_json::from_value(dolby_audio["backup_url"].take())
                                        .unwrap_or_default(),
                                    quality,
                                });
                            }
                        } else {
                            tracing::warn!("Dolby音频流缺少base_url或id字段");
                            dolby_failed = true;
                        }
                    }
                } else {
                    tracing::debug!("dolby音频数组为空，跳过dolby处理");
                }
            } else {
                tracing::debug!("未找到dolby音频数组");
            }
        }

        // 检查是否有音频流，如果没有且高品质音频失败，则回退到普通音频
        let has_audio = streams.iter().any(|s| matches!(s, Stream::DashAudio { .. }));
        if !has_audio && (hires_failed || dolby_failed) {
            tracing::warn!(
                "高品质音频不可用，尝试回退到普通音频流(132k/64k){}",
                if hires_failed && dolby_failed {
                    " (FLAC和Dolby均失败)"
                } else if hires_failed {
                    " (FLAC失败)"
                } else {
                    " (Dolby失败)"
                }
            );

            // 重新处理普通音频流，忽略质量限制，确保至少有一个可用音频
            if let Some(audios) = self.info.pointer_mut("/dash/audio").and_then(|a| a.as_array_mut()) {
                // 优先选择132k (30280) 或 64k (30216)
                let preferred_qualities = vec![30280_u64, 30216_u64];

                for preferred_q in preferred_qualities {
                    if let Some(audio) = audios.iter_mut().find(|a| a["id"].as_u64() == Some(preferred_q)) {
                        if let (Some(url), Some(quality)) = (audio["base_url"].as_str(), audio["id"].as_u64()) {
                            let quality = AudioQuality::from_repr(quality as usize)
                                .context(format!("invalid audio stream quality: {}", quality))?;
                            tracing::info!(
                                "已回退到普通音频流 - 质量: {:?} ({}k)",
                                quality,
                                if quality as usize == 30280 { "132" } else { "64" }
                            );
                            streams.push(Stream::DashAudio {
                                url: url.to_string(),
                                backup_url: serde_json::from_value(audio["backup_url"].take()).unwrap_or_default(),
                                quality,
                            });
                            break;
                        }
                    }
                }

                // 如果132k和64k都不可用，使用第一个可用的普通音频流
                if !streams.iter().any(|s| matches!(s, Stream::DashAudio { .. })) {
                    if let Some(audio) = audios.first_mut() {
                        if let (Some(url), Some(quality)) = (audio["base_url"].as_str(), audio["id"].as_u64()) {
                            let quality = AudioQuality::from_repr(quality as usize)
                                .context(format!("invalid audio stream quality: {}", quality))?;
                            tracing::info!("已回退到可用的普通音频流 - 质量: {:?}", quality);
                            streams.push(Stream::DashAudio {
                                url: url.to_string(),
                                backup_url: serde_json::from_value(audio["backup_url"].take()).unwrap_or_default(),
                                quality,
                            });
                        }
                    }
                }
            }
        }

        Ok(streams)
    }

    pub fn best_stream(&mut self, filter_option: &FilterOption) -> Result<BestStream> {
        let streams = self.streams(filter_option)?;
        if self.is_flv_stream() || self.is_html5_mp4_stream() || self.is_episode_try_mp4_stream() {
            // 按照 streams 中的假设，符合这三种情况的流只有一个，直接取
            return Ok(BestStream::Mixed(
                streams.into_iter().next().context("no stream found")?,
            ));
        }
        let (videos, audios): (Vec<Stream>, Vec<Stream>) =
            streams.into_iter().partition(|s| matches!(s, Stream::DashVideo { .. }));

        tracing::debug!("=== 最佳流选择 ===");
        if videos.is_empty() {
            tracing::error!("错误: 没有可用的视频流！");
            return Err(anyhow!("no video stream found"));
        }

        tracing::debug!(
            "候选流: {}",
            videos
                .iter()
                .filter_map(|s| match s {
                    Stream::DashVideo { quality, codecs, .. } =>
                        Some(format!("{:?}({}):{:?}", quality, *quality as u32, codecs)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(", ")
        );

        let selected_video = videos
            .into_iter()
            .max_by(|a, b| match (a, b) {
                (
                    Stream::DashVideo {
                        quality: a_quality,
                        codecs: a_codecs,
                        ..
                    },
                    Stream::DashVideo {
                        quality: b_quality,
                        codecs: b_codecs,
                        ..
                    },
                ) => {
                    // 优先按质量选择
                    if a_quality != b_quality {
                        return a_quality.cmp(b_quality);
                    }
                    // 质量相同时，按编码偏好选择
                    let a_pos = filter_option.codecs.iter().position(|c| c == a_codecs);
                    let b_pos = filter_option.codecs.iter().position(|c| c == b_codecs);
                    b_pos.cmp(&a_pos) // 优先选择更靠前的编码
                }
                _ => unreachable!(),
            })
            .context("no video stream found")?;

        if let Stream::DashVideo { quality, codecs, .. } = &selected_video {
            tracing::debug!("✓ 最终选择: {:?}({}) {:?}", quality, *quality as u32, codecs);
        }

        Ok(BestStream::VideoAudio {
            video: selected_video,
            audio: audios.into_iter().max_by(|a, b| match (a, b) {
                (Stream::DashAudio { quality: a_quality, .. }, Stream::DashAudio { quality: b_quality, .. }) => {
                    a_quality.cmp(b_quality)
                }
                _ => unreachable!(),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_quality_order() {
        assert!(VideoQuality::Quality360p < VideoQuality::Quality480p);
        assert!(VideoQuality::Quality480p < VideoQuality::Quality720p);
        assert!(VideoQuality::Quality720p < VideoQuality::Quality1080p);
        assert!(VideoQuality::Quality1080p < VideoQuality::Quality1080pPLUS);
        assert!(VideoQuality::Quality1080pPLUS < VideoQuality::Quality1080p60);
        assert!(VideoQuality::Quality1080p60 < VideoQuality::Quality4k);
        assert!(VideoQuality::Quality4k < VideoQuality::QualityHdr);
        assert!(VideoQuality::QualityHdr < VideoQuality::QualityDolby);
        assert!(VideoQuality::QualityDolby < VideoQuality::Quality8k);
    }

    #[test]
    fn test_video_quality_display() {
        assert_eq!(VideoQuality::Quality360p.to_string(), "360P流畅");
        assert_eq!(VideoQuality::Quality480p.to_string(), "480P清晰");
        assert_eq!(VideoQuality::Quality720p.to_string(), "720P高清");
        assert_eq!(VideoQuality::Quality1080p.to_string(), "1080P高清");
        assert_eq!(VideoQuality::Quality1080pPLUS.to_string(), "1080P+高码率");
        assert_eq!(VideoQuality::Quality1080p60.to_string(), "1080P 60fps");
        assert_eq!(VideoQuality::Quality4k.to_string(), "4K超高清");
        assert_eq!(VideoQuality::QualityHdr.to_string(), "HDR真彩");
        assert_eq!(VideoQuality::QualityDolby.to_string(), "杜比视界");
        assert_eq!(VideoQuality::Quality8k.to_string(), "8K超高清");
    }

    #[test]
    fn test_audio_quality_order() {
        assert!(AudioQuality::Quality64k < AudioQuality::Quality132k);
        assert!(AudioQuality::Quality132k < AudioQuality::Quality192k);
        assert!(AudioQuality::Quality192k < AudioQuality::QualityDolby);
        assert!(AudioQuality::Quality192k < AudioQuality::QualityHiRES);
        assert!(AudioQuality::QualityDolby < AudioQuality::QualityHiRES);
    }

    #[test]
    fn test_url_sort() {
        let urls = vec![
            "https://cn-xxx.com/video.mp4",
            "https://upos-xxx.com/video.mp4",
            "https://other-xxx.com/video.mp4",
        ];
        let mut sorted_urls = urls.clone();
        sorted_urls.sort_by_key(|u| {
            if u.contains("upos-") {
                0 // 服务商 cdn
            } else if u.contains("cn-") {
                1 // 国内 cdn
            } else {
                2 // 其他 cdn
            }
        });
        assert_eq!(
            sorted_urls,
            vec![
                "https://upos-xxx.com/video.mp4",
                "https://cn-xxx.com/video.mp4",
                "https://other-xxx.com/video.mp4",
            ]
        );
    }

    #[test]
    fn test_hdflv2_is_treated_as_flv_stream() {
        let mut analyzer = PageAnalyzer::new(json!({
            "format": "hdflv2",
            "durl": [{"url": "https://example.com/video.flv"}]
        }));

        let best = analyzer
            .best_stream(&FilterOption::default())
            .expect("should pick flv stream");
        match best {
            BestStream::Mixed(Stream::Flv(url)) => assert_eq!(url, "https://example.com/video.flv"),
            other => panic!("unexpected best stream: {:?}", other),
        }
    }

    #[test]
    fn test_missing_dash_video_is_not_risk_control() {
        let mut analyzer = PageAnalyzer::new(json!({
            "dash": {"audio": []}
        }));

        let err = analyzer.best_stream(&FilterOption::default()).unwrap_err();
        let bili_err = err.downcast_ref::<BiliError>().expect("should be BiliError");
        match bili_err {
            BiliError::VideoStreamEmpty(msg) => assert!(msg.contains("dash/video")),
            other => panic!("unexpected bili error: {:?}", other),
        }
    }
}
