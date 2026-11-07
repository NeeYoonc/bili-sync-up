use std::fmt::Display;

use crate::utils::subtitle::{
    bilingual_ratio, clean_cue_text, merge_overlay, parse_bilingual_language_pair, primary_language, render_srt,
    SubtitleCue,
};

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct SubTitlesInfo {
    pub subtitles: Vec<SubTitleInfo>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SubTitleInfo {
    pub lan: String,
    pub subtitle_url: String,
}

pub struct SubTitle {
    pub lan: String,
    pub body: SubTitleBody,
}

pub const DEFAULT_AI_SUBTITLE_LANGUAGE: &str = "zh-CN";

pub struct SubtitleDownloadOptions {
    pub download_ai_subtitle: bool,
    pub ai_subtitle_language: String,
}

impl Default for SubtitleDownloadOptions {
    fn default() -> Self {
        Self {
            download_ai_subtitle: true,
            ai_subtitle_language: DEFAULT_AI_SUBTITLE_LANGUAGE.to_string(),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct SubTitleBody(pub Vec<SubTitleItem>);

#[derive(Debug, serde::Deserialize)]
pub struct SubTitleItem {
    from: f64,
    to: f64,
    content: String,
}

impl SubTitleInfo {
    pub fn is_ai_sub(&self) -> bool {
        // ai： aisubtitle.hdslb.com/bfs/ai_subtitle/xxxx
        // 非 ai： aisubtitle.hdslb.com/bfs/subtitle/xxxx
        // 两个判据取并集：`lan` 前缀是接口语义（AI 轨一律以 `ai-` 开头），
        // URL 是历史实现依赖的存储路径，任一命中都视为 AI 轨，避免 CDN 路径调整后漏判。
        self.lan.trim().to_ascii_lowercase().starts_with("ai-") || self.subtitle_url.contains("ai_subtitle")
    }

    /// 是否拿到了可下载的地址。
    ///
    /// 接口偶发会整批返回空 `subtitle_url`（风控或降级），这种轨下载必然失败；
    /// 提前剔除，避免一条坏轨把整个字幕子任务拖成失败。
    pub fn is_downloadable(&self) -> bool {
        let url = self.subtitle_url.trim();
        url.starts_with("//") || url.starts_with("http://") || url.starts_with("https://")
    }
}

impl SubTitleBody {
    /// 把接口返回的字幕条目转成统一的 cue 列表（秒 -> 毫秒，顺带清洗内联标签）。
    pub(crate) fn to_cues(&self) -> Vec<SubtitleCue> {
        let mut cues: Vec<SubtitleCue> = self
            .0
            .iter()
            .filter_map(|item| {
                let start_ms = (item.from * 1000.0).round() as i64;
                let end_ms = (item.to * 1000.0).round() as i64;
                let text = clean_cue_text(&item.content);
                if text.is_empty() {
                    return None;
                }
                Some(SubtitleCue {
                    start_ms,
                    end_ms: end_ms.max(start_ms),
                    text,
                })
            })
            .collect();
        cues.sort_by_key(|cue| cue.start_ms);
        cues
    }
}

/// 由两条已下载字幕轨合并出的双语字幕：文件名语言后缀 + SRT 正文。
pub struct BilingualSubtitle {
    pub lan: String,
    pub body: String,
}

/// 双语合并的结果。
pub enum BilingualMerge {
    /// 合并出一条新的双语字幕，需要额外落盘。
    Merged(BilingualSubtitle),
    /// 目标语言轨本身已是双语（例如 UP 主上传的中英对照字幕），
    /// 再生成一份只是重复，保留原有文件即可。
    AlreadyBilingual { lan: String, ratio: f32 },
    /// 缺轨、空轨或时间轴无交集，无法合并。
    Unavailable,
}

/// 判定「这条轨已经是双语」的阈值：九成以上 cue 已含对照语言。
const ALREADY_BILINGUAL_RATIO: f32 = 0.9;

/// 把 `subtitles` 里的目标语言轨与对照语言轨按时间轴重叠度合并成一条双语字幕。
///
/// 时间轴以目标语言轨为准，对照语言（通常是原文）叠在上方，与 YouTube 侧的双语
/// 输出保持一致：英文在上、中文在下。缺轨或空轨时返回 `Unavailable`，由调用方降级
/// 为「只落盘分轨文件」，不影响原有行为。
pub fn merge_bilingual_subtitles(subtitles: &[SubTitle], primary: &str, secondary: &str) -> BilingualMerge {
    let pick = |language: &str| {
        subtitles
            .iter()
            .find(|subtitle| language_matches(&subtitle.lan, language))
    };
    let Some(primary_track) = pick(primary) else {
        return BilingualMerge::Unavailable;
    };
    let Some(secondary_track) = pick(secondary) else {
        return BilingualMerge::Unavailable;
    };
    let timeline = primary_track.body.to_cues();
    let overlay = secondary_track.body.to_cues();
    if timeline.is_empty() || overlay.is_empty() {
        return BilingualMerge::Unavailable;
    }
    // 目标轨本身已是双语（B 站大量「中英对照」字幕属于这种），不必再出一份重复文件。
    let ratio = bilingual_ratio(&timeline, secondary);
    if ratio >= ALREADY_BILINGUAL_RATIO {
        return BilingualMerge::AlreadyBilingual {
            lan: primary_track.lan.clone(),
            ratio,
        };
    }
    let merged = merge_overlay(&timeline, &overlay, Some(secondary));
    if merged.is_empty() {
        return BilingualMerge::Unavailable;
    }
    BilingualMerge::Merged(BilingualSubtitle {
        lan: format!(
            "{}-{}",
            subtitle_file_tag(&primary_track.lan),
            subtitle_file_tag(&secondary_track.lan)
        ),
        body: render_srt(&merged),
    })
}

/// 生成文件名里的语言后缀：`ai-zh` / `zh-CN` 一律归一成 `zh-CN`，其余保持两位小写码。
fn subtitle_file_tag(language: &str) -> String {
    match normalize_language(language).as_str() {
        "zh-cn" => "zh-CN".to_string(),
        other => other.to_string(),
    }
}

impl SubTitlesInfo {
    pub fn into_downloadable_subtitles(self, options: &SubtitleDownloadOptions) -> Vec<SubTitleInfo> {
        let mut seen_languages = std::collections::HashSet::new();
        let mut manual_subtitles = Vec::new();
        let mut ai_subtitles = Vec::new();

        for subtitle in self.subtitles {
            if !subtitle.is_downloadable() {
                continue;
            }
            if subtitle.is_ai_sub() {
                ai_subtitles.push(subtitle);
            } else if seen_languages.insert(normalize_language(&subtitle.lan)) {
                manual_subtitles.push(subtitle);
            }
        }

        if options.download_ai_subtitle {
            // B 站字幕接口只认单一语言码，双语设置（如 `zh-CN+en`）要先剥成目标语言。
            let requested_language = primary_language(&options.ai_subtitle_language);
            let selected_ai_subtitle = find_ai_subtitle_by_language(&ai_subtitles, requested_language)
                .or_else(|| find_ai_subtitle_by_language(&ai_subtitles, DEFAULT_AI_SUBTITLE_LANGUAGE));

            if let Some(subtitle) = selected_ai_subtitle {
                if seen_languages.insert(normalize_language(&subtitle.lan)) {
                    manual_subtitles.push(subtitle);
                }
            }

            // 双语设置还要把对照语言的 AI 轨一并取回，否则上游只能拿到单语。
            if let Some((_, secondary)) = parse_bilingual_language_pair(&options.ai_subtitle_language) {
                if !language_matches(&secondary, requested_language) {
                    if let Some(subtitle) = find_ai_subtitle_by_language(&ai_subtitles, &secondary) {
                        if seen_languages.insert(normalize_language(&subtitle.lan)) {
                            manual_subtitles.push(subtitle);
                        }
                    }
                }
            }
        }

        manual_subtitles
    }
}

fn find_ai_subtitle_by_language(subtitles: &[SubTitleInfo], language: &str) -> Option<SubTitleInfo> {
    subtitles
        .iter()
        .find(|subtitle| language_matches(&subtitle.lan, language))
        .cloned()
}

fn language_matches(actual: &str, requested: &str) -> bool {
    normalize_language(actual) == normalize_language(requested)
}

fn normalize_language(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "zh" | "zh-cn" | "zh-hans" | "cmn-hans" | "ai-zh" | "ai-zh-cn" => "zh-cn".to_string(),
        "en" | "en-us" | "en-gb" | "ai-en" => "en".to_string(),
        "ja" | "ja-jp" | "jp" | "ai-ja" | "ai-jp" => "ja".to_string(),
        "ko" | "ko-kr" | "kr" | "ai-ko" | "ai-kr" => "ko".to_string(),
        other => other.to_string(),
    }
}

impl Display for SubTitleBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (idx, item) in self.0.iter().enumerate() {
            writeln!(f, "{}", idx)?;
            writeln!(f, "{} --> {}", format_time(item.from), format_time(item.to))?;
            writeln!(f, "{}", item.content)?;
            writeln!(f)?;
        }
        Ok(())
    }
}

fn format_time(time: f64) -> String {
    let (second, millisecond) = (time.trunc(), (time.fract() * 1e3) as u32);
    let (hour, minute, second) = (
        (second / 3600.0) as u32,
        ((second % 3600.0) / 60.0) as u32,
        (second % 60.0) as u32,
    );
    format!("{:02}:{:02}:{:02},{:03}", hour, minute, second, millisecond)
}

#[cfg(test)]
mod tests {
    /// 取出合并结果，非 `Merged` 直接失败，便于断言。
    fn merged_of(outcome: super::BilingualMerge) -> super::BilingualSubtitle {
        match outcome {
            super::BilingualMerge::Merged(merged) => merged,
            _ => panic!("期望合并出一条新的双语字幕"),
        }
    }

    #[test]
    fn test_format_time() {
        // float 解析会有精度问题，但误差几毫秒应该不太关键
        // 想再健壮一点就得手写 serde_json 解析拆分秒和毫秒，然后分别处理了
        let testcases = [
            (0.0, "00:00:00,000"),
            (1.5, "00:00:01,500"),
            (206.45, "00:03:26,449"),
            (360001.23, "100:00:01,229"),
        ];
        for (time, expect) in testcases.iter() {
            assert_eq!(super::format_time(*time), *expect);
        }
    }

    #[test]
    fn downloadable_subtitles_include_ai_subtitles() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ja-JP",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/subtitle/manual.json"
                },
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/ai.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "ai-zh".to_string(),
        });

        assert_eq!(subtitles.len(), 2);
        assert!(subtitles
            .iter()
            .any(|subtitle| subtitle.subtitle_url.contains("/subtitle/")));
        assert!(subtitles
            .iter()
            .any(|subtitle| subtitle.subtitle_url.contains("/ai_subtitle/")));
    }

    #[test]
    fn downloadable_subtitles_prefer_manual_when_language_duplicates() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "zh-CN",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/ai.json"
                },
                {
                    "lan": "zh-CN",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/subtitle/manual.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "zh-CN".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert!(subtitles[0].subtitle_url.contains("/subtitle/"));
    }

    #[test]
    fn downloadable_subtitles_skip_ai_when_disabled() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "zh-CN",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/subtitle/manual.json"
                },
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/ai.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: false,
            ai_subtitle_language: "ai-zh".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert!(subtitles[0].subtitle_url.contains("/subtitle/"));
    }

    #[test]
    fn downloadable_subtitles_use_requested_ai_language() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/zh.json"
                },
                {
                    "lan": "ai-en",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/en.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "ai-en".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "ai-en");
    }

    #[test]
    fn downloadable_subtitles_fallback_to_chinese_ai_language() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/zh.json"
                },
                {
                    "lan": "ai-ja",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/ja.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "ai-en".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "ai-zh");
    }

    #[test]
    fn downloadable_subtitles_treat_zh_cn_as_ai_zh() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/zh.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "zh-CN".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "ai-zh");
    }

    #[test]
    fn downloadable_subtitles_treat_en_us_as_ai_en() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/zh.json"
                },
                {
                    "lan": "ai-en",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/en.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "en-US".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "ai-en");
    }

    fn item(from: f64, to: f64, content: &str) -> super::SubTitleItem {
        super::SubTitleItem {
            from,
            to,
            content: content.to_string(),
        }
    }

    fn track(lan: &str, items: Vec<super::SubTitleItem>) -> super::SubTitle {
        super::SubTitle {
            lan: lan.to_string(),
            body: super::SubTitleBody(items),
        }
    }

    #[test]
    fn tracks_without_download_url_are_dropped() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": ""
                },
                {
                    "lan": "ai-en",
                    "subtitle_url": ""
                },
                {
                    "lan": "zh-CN",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/subtitle/manual.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "zh-CN+en".to_string(),
        });

        // 空地址的 AI 轨会被剔除，也不会去补中文兜底轨（它同样没有地址）。
        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "zh-CN");
    }

    #[test]
    fn ai_subtitle_is_detected_by_lan_prefix_even_with_unknown_url() {
        let by_lan: super::SubTitleInfo = serde_json::from_value(serde_json::json!({
            "lan": "ai-zh",
            "subtitle_url": "//aisubtitle.hdslb.com/bfs/subtitle/legacy.json"
        }))
        .unwrap();
        assert!(by_lan.is_ai_sub());

        let by_url: super::SubTitleInfo = serde_json::from_value(serde_json::json!({
            "lan": "zh-CN",
            "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/legacy.json"
        }))
        .unwrap();
        assert!(by_url.is_ai_sub());

        let manual: super::SubTitleInfo = serde_json::from_value(serde_json::json!({
            "lan": "en-US",
            "subtitle_url": "//aisubtitle.hdslb.com/bfs/subtitle/manual.json"
        }))
        .unwrap();
        assert!(!manual.is_ai_sub());
    }

    #[test]
    fn bilingual_request_also_pulls_secondary_ai_track() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/zh.json"
                },
                {
                    "lan": "ai-en",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/en.json"
                },
                {
                    "lan": "ai-ja",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/ja.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "zh-CN+en".to_string(),
        });

        let languages: Vec<&str> = subtitles.iter().map(|subtitle| subtitle.lan.as_str()).collect();
        assert_eq!(languages, vec!["ai-zh", "ai-en"]);
    }

    #[test]
    fn bilingual_request_keeps_single_track_when_secondary_matches_primary() {
        let subtitles_info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                {
                    "lan": "ai-zh",
                    "subtitle_url": "//aisubtitle.hdslb.com/bfs/ai_subtitle/zh.json"
                }
            ]
        }))
        .unwrap();

        let subtitles = subtitles_info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "zh-CN+zh-Hans".to_string(),
        });

        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "ai-zh");
    }

    #[test]
    fn merge_bilingual_subtitles_aligns_by_timeline_overlap() {
        let primary = track(
            "zh-CN",
            vec![item(0.0, 2.0, "第一句中文"), item(2.0, 4.0, "第二句中文")],
        );
        // 对照轨切得更碎：两条英文都落在第一句中文的时间范围内。
        let secondary = track(
            "en-US",
            vec![
                item(0.0, 0.9, "first"),
                item(0.9, 2.0, "part two"),
                item(2.0, 4.0, "second"),
            ],
        );

        let merged = merged_of(super::merge_bilingual_subtitles(&[primary, secondary], "zh-CN", "en"));

        assert_eq!(merged.lan, "zh-CN-en");
        assert!(merged
            .body
            .starts_with("1\n00:00:00,000 --> 00:00:02,000\npart two\n第一句中文\n\n"));
        assert!(merged
            .body
            .contains("2\n00:00:02,000 --> 00:00:04,000\nsecond\n第二句中文\n"));
    }

    #[test]
    fn merge_bilingual_subtitles_matches_ai_language_alias() {
        let primary = track("ai-zh", vec![item(0.0, 1.0, "中文")]);
        let secondary = track("ai-en", vec![item(0.0, 1.0, "english")]);

        let merged = merged_of(super::merge_bilingual_subtitles(&[primary, secondary], "zh-CN", "en"));

        assert_eq!(merged.lan, "zh-CN-en");
        assert!(merged.body.contains("english\n中文"));
    }

    #[test]
    fn merge_bilingual_subtitles_degrades_when_secondary_missing() {
        let primary = track("zh-CN", vec![item(0.0, 1.0, "只有中文")]);

        assert!(matches!(
            super::merge_bilingual_subtitles(&[primary], "zh-CN", "en"),
            super::BilingualMerge::Unavailable
        ));
    }

    #[test]
    fn merge_bilingual_subtitles_degrades_when_track_is_empty() {
        let primary = track("zh-CN", vec![item(0.0, 1.0, "中文")]);
        let secondary = track("en-US", vec![item(0.0, 1.0, "   ")]);

        assert!(matches!(
            super::merge_bilingual_subtitles(&[primary, secondary], "zh-CN", "en"),
            super::BilingualMerge::Unavailable
        ));
    }

    /// 目标轨本身就是中英对照（B 站大量 UP 主上传的字幕属于这种）：不应再产出重复文件。
    #[test]
    fn merge_bilingual_subtitles_reports_already_bilingual_track() {
        let primary = track(
            "zh",
            vec![
                item(0.0, 2.0, "噢，不！\nOh, no."),
                item(2.0, 4.0, "(鼹鼠家)\n(Mole End)"),
            ],
        );
        let secondary = track("en", vec![item(0.0, 2.0, "Oh, no."), item(2.0, 4.0, "(Mole End)")]);

        match super::merge_bilingual_subtitles(&[primary, secondary], "zh-CN", "en") {
            super::BilingualMerge::AlreadyBilingual { lan, ratio } => {
                assert_eq!(lan, "zh");
                assert!(ratio >= 0.9, "ratio={ratio}");
            }
            _ => panic!("应识别为已经是双语轨"),
        }
    }

    /// 只有部分 cue 自带对照语言时仍要合并，但不能让同一句英文出现两次。
    #[test]
    fn merge_bilingual_subtitles_does_not_duplicate_existing_lines() {
        let primary = track(
            "zh",
            vec![
                item(0.0, 2.0, "噢，不！\nOh, no."),
                item(2.0, 4.0, "第一句中文"),
                item(4.0, 6.0, "第二句中文"),
            ],
        );
        let secondary = track(
            "en",
            vec![
                item(0.0, 2.0, "Oh, no."),
                item(2.0, 4.0, "first line"),
                item(4.0, 6.0, "second line"),
            ],
        );

        let merged = merged_of(super::merge_bilingual_subtitles(&[primary, secondary], "zh-CN", "en"));
        // 已带英文的第一句保持原样，不重复叠加。
        assert!(merged.body.contains("噢，不！\nOh, no.\n\n"), "body=\n{}", merged.body);
        assert!(!merged.body.contains("Oh, no.\n噢，不"));
        // 缺英文的两句正常补上。
        assert!(merged.body.contains("first line\n第一句中文"));
        assert!(merged.body.contains("second line\n第二句中文"));
    }
}
