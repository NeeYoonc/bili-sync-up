use std::fmt::Display;

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
        self.subtitle_url.contains("ai_subtitle")
    }
}

impl SubTitlesInfo {
    pub fn into_downloadable_subtitles(self, options: &SubtitleDownloadOptions) -> Vec<SubTitleInfo> {
        let mut seen_languages = std::collections::HashSet::new();
        let mut manual_subtitles = Vec::new();
        let mut ai_subtitles = Vec::new();

        for subtitle in self.subtitles {
            if subtitle.is_ai_sub() {
                ai_subtitles.push(subtitle);
            } else if seen_languages.insert(normalize_language(&subtitle.lan)) {
                manual_subtitles.push(subtitle);
            }
        }

        if options.download_ai_subtitle {
            let requested_language = options.ai_subtitle_language.trim();
            let selected_ai_subtitle = find_ai_subtitle_by_language(&ai_subtitles, requested_language)
                .or_else(|| find_ai_subtitle_by_language(&ai_subtitles, DEFAULT_AI_SUBTITLE_LANGUAGE));

            if let Some(subtitle) = selected_ai_subtitle {
                if seen_languages.insert(normalize_language(&subtitle.lan)) {
                    manual_subtitles.push(subtitle);
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
}
