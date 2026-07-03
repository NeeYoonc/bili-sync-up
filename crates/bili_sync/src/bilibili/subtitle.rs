use std::fmt::Display;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct SubTitlesInfo {
    pub subtitles: Vec<SubTitleInfo>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SubTitleInfo {
    pub lan: String,
    pub subtitle_url: String,
}

pub struct SubTitle {
    pub lan: String,
    pub body: SubTitleBody,
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
    pub fn into_downloadable_subtitles(self) -> Vec<SubTitleInfo> {
        let mut seen_languages = std::collections::HashSet::new();
        let mut manual_subtitles = Vec::new();
        let mut ai_subtitles = Vec::new();

        for subtitle in self.subtitles {
            if subtitle.is_ai_sub() {
                ai_subtitles.push(subtitle);
            } else if seen_languages.insert(subtitle.lan.clone()) {
                manual_subtitles.push(subtitle);
            }
        }

        for subtitle in ai_subtitles {
            if seen_languages.insert(subtitle.lan.clone()) {
                manual_subtitles.push(subtitle);
            }
        }

        manual_subtitles
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

        let subtitles = subtitles_info.into_downloadable_subtitles();

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

        let subtitles = subtitles_info.into_downloadable_subtitles();

        assert_eq!(subtitles.len(), 1);
        assert!(subtitles[0].subtitle_url.contains("/subtitle/"));
    }
}
