//! 字幕文本处理：WebVTT / SRT 解析、ASR 内联标签清洗、双语字幕合并。
//!
//! YouTube 返回的字幕是平台侧的原始文件：既可能是人工上传的对照字幕，也可能是
//! 带 `<c>` / `<00:00:01.000>` 内联时间戳的自动识别（ASR）字幕。这里只做三件事：
//! 解析成 cue、清洗文本、把两条轨按时间轴重叠度合并成一条双语字幕。
//!
//! 这里刻意不引入额外依赖，也不做样式/位置信息的保留——输出是给播放器与
//! 刮削器消费的纯文本双语轨。

/// 一条字幕：起止时间（毫秒）+ 已清洗的文本（多行用 `\n` 分隔）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubtitleCue {
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    pub(crate) text: String,
}

/// 解析 WebVTT / SRT 文本为 cue 列表。
///
/// 容忍两种时间戳写法（`00:00:01.000` 与 SRT 的 `00:00:01,000`）、容忍省略小时的
/// `01:02.500`、忽略 `NOTE` / `STYLE` / 头部元信息与 SRT 的序号行。
pub(crate) fn parse_cues(input: &str) -> Vec<SubtitleCue> {
    let mut cues = Vec::new();
    let mut pending: Option<(i64, i64)> = None;
    let mut lines: Vec<String> = Vec::new();

    let flush = |cues: &mut Vec<SubtitleCue>, pending: &mut Option<(i64, i64)>, lines: &mut Vec<String>| {
        if let Some((start_ms, end_ms)) = pending.take() {
            let text = clean_cue_lines(lines);
            lines.clear();
            if !text.is_empty() {
                cues.push(SubtitleCue { start_ms, end_ms, text });
            }
        } else {
            lines.clear();
        }
    };

    for raw in input.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if let Some((left, right)) = line.split_once("-->") {
            flush(&mut cues, &mut pending, &mut lines);
            let end_token = right.split_whitespace().next().unwrap_or_default();
            if let (Some(start_ms), Some(end_ms)) = (parse_timestamp(left), parse_timestamp(end_token)) {
                pending = Some((start_ms, end_ms.max(start_ms)));
            }
            continue;
        }
        if line.is_empty() {
            flush(&mut cues, &mut pending, &mut lines);
            continue;
        }
        if line.starts_with("WEBVTT")
            || line.starts_with("NOTE")
            || line.starts_with("STYLE")
            || line.starts_with("REGION")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
        {
            continue;
        }
        // SRT 的序号行：只在还没有时间戳时出现，且必须是纯数字。
        if pending.is_none() && lines.is_empty() && line.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        lines.push(line.to_string());
    }
    flush(&mut cues, &mut pending, &mut lines);

    cues.sort_by_key(|cue| cue.start_ms);
    cues
}

/// 把 cue 列表渲染成 SRT 文本（B 站字幕落盘格式）。
pub(crate) fn render_srt(cues: &[SubtitleCue]) -> String {
    let mut output = String::with_capacity(cues.len().saturating_mul(96) + 16);
    for (index, cue) in cues.iter().enumerate() {
        output.push_str(&(index + 1).to_string());
        output.push('\n');
        output.push_str(&render_srt_timestamp(cue.start_ms));
        output.push_str(" --> ");
        output.push_str(&render_srt_timestamp(cue.end_ms));
        output.push('\n');
        for line in cue.text.lines() {
            output.push_str(line);
            output.push('\n');
        }
        output.push('\n');
    }
    output
}

/// 把 cue 列表渲染成 ASS 文本（YouTube 字幕落盘格式之一）。
///
/// 与本模块的整体定位一致：不做样式/位置信息的保留，输出默认样式的
/// 纯文本轨，保证 Jellyfin/Emby 等媒体库能直接识别 `.ass` 后缀。
/// cue 内多行文本用 ASS 的软换行 `\N` 连接。
pub(crate) fn render_ass(cues: &[SubtitleCue]) -> String {
    let mut output = String::with_capacity(cues.len().saturating_mul(112) + 512);
    output.push_str("[Script Info]\n");
    output.push_str("ScriptType: v4.00+\n");
    output.push_str("PlayResX: 384\n");
    output.push_str("PlayResY: 288\n");
    output.push_str("WrapStyle: 0\n");
    output.push_str("ScaledBorderAndShadow: yes\n");
    output.push('\n');
    output.push_str("[V4+ Styles]\n");
    output.push_str(
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    );
    output.push_str(
        "Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1\n",
    );
    output.push('\n');
    output.push_str("[Events]\n");
    output.push_str("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    for cue in cues {
        output.push_str("Dialogue: 0,");
        output.push_str(&render_ass_timestamp(cue.start_ms));
        output.push(',');
        output.push_str(&render_ass_timestamp(cue.end_ms));
        output.push_str(",Default,,0,0,0,,");
        output.push_str(&escape_ass_text(&cue.text));
        output.push('\n');
    }
    output
}

/// 转义 ASS 正文：`{}` 是 override 标签边界，`\` 是转义引导符，正文里出现时必须
/// 加反斜杠，否则播放器会把整段内容当成样式指令吃掉（例如字幕文本里的「{笑}」）。
/// 换行转成 ASS 的软换行 `\N`。
fn escape_ass_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '\\' | '{' | '}' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            '\n' => escaped.push_str("\\N"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// 清洗一段已按行拆分好的字幕文本（用于平台直接给出 `content` 字段、而非整份文件的场景）。
pub(crate) fn clean_cue_text(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    clean_cue_lines(&lines)
}

/// 语言对应的文字系统，用于判断「这句是不是已经是目标语言了」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScriptKind {
    Han,
    Kana,
    Hangul,
    Latin,
    Cyrillic,
    Arabic,
    Thai,
    /// 不认识的语言：不做猜测，避免误判。
    Unknown,
}

fn script_kind_of(language: &str) -> ScriptKind {
    // `ai-zh` / `zh-CN` / `zh_Hans` 这类写法统一折算到基本语言码。
    let lowered = language.trim().to_ascii_lowercase();
    let lowered = lowered.strip_prefix("ai-").unwrap_or(&lowered);
    let base = lowered.split(['-', '_']).next().unwrap_or_default();
    match base {
        "zh" | "cmn" | "yue" | "wuu" | "nan" => ScriptKind::Han,
        "ja" | "jp" => ScriptKind::Kana,
        "ko" | "kr" => ScriptKind::Hangul,
        "ru" | "uk" | "be" | "bg" | "sr" | "mk" => ScriptKind::Cyrillic,
        "ar" | "fa" | "ur" | "ps" => ScriptKind::Arabic,
        "th" => ScriptKind::Thai,
        "en" | "es" | "fr" | "de" | "pt" | "it" | "nl" | "pl" | "tr" | "id" | "ms" | "vi" | "ro" | "cs" | "sv"
        | "da" | "fi" | "no" | "hu" | "tl" | "fil" => ScriptKind::Latin,
        _ => ScriptKind::Unknown,
    }
}

fn char_in_script(c: char, script: ScriptKind) -> bool {
    let code = c as u32;
    match script {
        ScriptKind::Han => matches!(code, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF),
        // 日文一行里汉字和假名都可能出现，两者都算命中。
        ScriptKind::Kana => matches!(code, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0x4E00..=0x9FFF),
        ScriptKind::Hangul => matches!(code, 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF),
        ScriptKind::Cyrillic => matches!(code, 0x0400..=0x04FF),
        ScriptKind::Arabic => matches!(code, 0x0600..=0x06FF | 0x0750..=0x077F),
        ScriptKind::Thai => matches!(code, 0x0E00..=0x0E7F),
        ScriptKind::Latin => c.is_ascii_alphabetic(),
        ScriptKind::Unknown => false,
    }
}

/// 单行文本是否「主要由 `language` 的正文文字构成」。
///
/// 判据是过半的字母/数字落在该语言的文字系统内，因此 `(Mole End)` 算英文、
/// `(鼹鼠家)` 算中文、`第 2 集 ABC` 因为英文占比不足不算英文。
pub(crate) fn line_matches_language(line: &str, language: &str) -> bool {
    let script = script_kind_of(language);
    if script == ScriptKind::Unknown {
        return false;
    }
    let mut in_script = 0usize;
    let mut word_chars = 0usize;
    for c in line.chars() {
        if !c.is_alphanumeric() {
            continue;
        }
        word_chars += 1;
        if char_in_script(c, script) {
            in_script += 1;
        }
    }
    in_script >= 2 && in_script * 2 >= word_chars
}

/// 多行字幕文本里是否已经有 `language` 的行。
pub(crate) fn text_contains_language(text: &str, language: &str) -> bool {
    text.lines().any(|line| line_matches_language(line, language))
}

/// cue 列表里已含 `language` 的比例（0.0 ~ 1.0），空列表返回 0。
///
/// 用来判断「这条轨本身是不是双语轨」：比例接近 1 时说明对照语言已经内嵌在文本里，
/// 再合并只会得到一份重复产物。
pub(crate) fn bilingual_ratio(timeline: &[SubtitleCue], language: &str) -> f32 {
    if timeline.is_empty() {
        return 0.0;
    }
    let hits = timeline
        .iter()
        .filter(|cue| text_contains_language(&cue.text, language))
        .count();
    hits as f32 / timeline.len() as f32
}

/// 把 `overlay` 的文本叠到 `timeline` 之上，生成双语字幕。
///
/// 时间轴以 `timeline` 为准（通常是目标语言轨，切分更贴合阅读节奏），`overlay`
/// 只取与某个 cue **时间重叠最多** 的那条文本，避免两条轨行数不同导致的中英错位。
///
/// `overlay_language` 用来去重：上游不少「中文字幕」其实已经是中英对照（UP 主上传的
/// 双语 srt、YouTube 人工双语轨），此时再叠一遍对照轨会让英文出现两次。所以只要该
/// cue 的文本里已经有对照语言的行，就跳过叠加。
pub(crate) fn merge_overlay(
    timeline: &[SubtitleCue],
    overlay: &[SubtitleCue],
    overlay_language: Option<&str>,
) -> Vec<SubtitleCue> {
    let mut merged = Vec::with_capacity(timeline.len());
    for cue in timeline {
        let overlay_text = best_overlap_text(cue, overlay);
        let mut text = String::new();
        if let Some(overlay_text) = overlay_text {
            let already_present = overlay_text.is_empty()
                || overlay_text == cue.text
                || overlay_language
                    .map(|language| text_contains_language(&cue.text, language))
                    .unwrap_or(false);
            if !already_present {
                text.push_str(overlay_text);
            }
        }
        if !text.is_empty() && !cue.text.is_empty() {
            text.push('\n');
        }
        if !cue.text.is_empty() {
            text.push_str(&cue.text);
        }
        if text.is_empty() {
            continue;
        }
        merged.push(SubtitleCue {
            start_ms: cue.start_ms,
            end_ms: cue.end_ms,
            text,
        });
    }
    merged
}

/// 解析双语字幕请求：`zh-CN+en` 表示「中文为目标语言，英文作为上方对照行」。
pub(crate) fn parse_bilingual_language_pair(value: &str) -> Option<(String, String)> {
    let (primary, secondary) = value.split_once('+')?;
    let primary = primary.trim();
    let secondary = secondary.trim();
    if primary.is_empty() || secondary.is_empty() {
        return None;
    }
    Some((primary.to_string(), secondary.to_string()))
}

/// 取语言设置里的目标语言部分：`zh-CN+en` -> `zh-CN`，`zh-CN` -> `zh-CN`。
///
/// B 站字幕接口只认单一语言码，双语后缀必须在这里被剥掉，否则会查不到字幕。
pub(crate) fn primary_language(value: &str) -> &str {
    let trimmed = value.trim();
    match trimmed.split_once('+') {
        Some((primary, _)) if !primary.trim().is_empty() => primary.trim(),
        _ => trimmed,
    }
}

fn best_overlap_text<'a>(cue: &SubtitleCue, overlay: &'a [SubtitleCue]) -> Option<&'a str> {
    let mut best: Option<(i64, &'a SubtitleCue)> = None;
    for candidate in overlay {
        let start = cue.start_ms.max(candidate.start_ms);
        let end = cue.end_ms.min(candidate.end_ms);
        let overlap = end - start;
        if overlap > 0 && best.map(|(score, _)| overlap > score).unwrap_or(true) {
            best = Some((overlap, candidate));
        }
    }
    best.map(|(_, candidate)| candidate.text.as_str())
}

fn clean_cue_lines(lines: &[String]) -> String {
    let mut kept: Vec<String> = Vec::new();
    for line in lines {
        let cleaned = clean_caption_line(line);
        if cleaned.is_empty() {
            continue;
        }
        if kept.last().map(|previous| previous == &cleaned).unwrap_or(false) {
            continue;
        }
        kept.push(cleaned);
    }
    kept.join("\n")
}

/// 清洗单行字幕文本：去掉内联标签（`<c>`、`<00:00:01.000>` 等）并还原实体。
fn clean_caption_line(line: &str) -> String {
    let without_tags = strip_inline_tags(line);
    let unescaped = unescape_entities(&without_tags);
    unescaped.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_inline_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut inside_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' if inside_tag => inside_tag = false,
            _ if !inside_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn unescape_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(end) = tail[..tail.len().min(12)].find(';') else {
            output.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity.strip_prefix('#').and_then(|digits| {
                let code = if let Some(hex) = digits.strip_prefix('x').or_else(|| digits.strip_prefix('X')) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    digits.parse::<u32>().ok()
                };
                code.and_then(char::from_u32)
            }),
        };
        match replacement {
            Some(ch) => {
                output.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                output.push('&');
                rest = &tail[1..];
            }
        }
    }
    output.push_str(rest);
    output
}

fn parse_timestamp(value: &str) -> Option<i64> {
    let cleaned = value.trim().replace(',', ".");
    if cleaned.is_empty() {
        return None;
    }
    let pieces: Vec<&str> = cleaned.split(':').collect();
    if pieces.len() > 3 {
        return None;
    }
    let last = pieces.last()?;
    let (seconds_text, millis_text) = match last.split_once('.') {
        Some((seconds, millis)) => (seconds, Some(millis)),
        None => (*last, None),
    };
    if seconds_text.is_empty() || !seconds_text.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let mut total = seconds_text.parse::<i64>().ok()?.checked_mul(1000)?;
    if let Some(millis) = millis_text {
        if !millis.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let mut padded = millis.to_string();
        while padded.len() < 3 {
            padded.push('0');
        }
        total = total.checked_add(padded[..3].parse::<i64>().ok()?)?;
    }
    let mut multiplier = 60_000i64;
    for piece in pieces[..pieces.len() - 1].iter().rev() {
        if piece.is_empty() || !piece.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        total = total.checked_add(piece.parse::<i64>().ok()?.checked_mul(multiplier)?)?;
        multiplier = multiplier.checked_mul(60)?;
    }
    Some(total)
}

fn render_srt_timestamp(milliseconds: i64) -> String {
    let value = milliseconds.max(0);
    let hours = value / 3_600_000;
    let minutes = (value % 3_600_000) / 60_000;
    let seconds = (value % 60_000) / 1000;
    let millis = value % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

/// ASS 时间戳：`H:MM:SS.CS`（小时不补零，精确到百分之一秒）。
fn render_ass_timestamp(milliseconds: i64) -> String {
    let value = milliseconds.max(0);
    let hours = value / 3_600_000;
    let minutes = (value % 3_600_000) / 60_000;
    let seconds = (value % 60_000) / 1000;
    let centis = (value % 1000) / 10;
    format!("{hours}:{minutes:02}:{seconds:02}.{centis:02}")
}

#[cfg(test)]
mod tests {
    use super::{
        clean_cue_text, merge_overlay, parse_bilingual_language_pair, parse_cues, primary_language, render_srt,
        SubtitleCue,
    };

    #[test]
    fn parses_vtt_and_strips_asr_inline_tags() {
        let input = "WEBVTT\nKind: captions\nLanguage: en\n\n\
            00:00:01.000 --> 00:00:03.500\n\
            good<00:00:01.200><c> morning</c>\n\
            good morning\n\n\
            00:00:03.500 --> 00:00:05.000\n\
            how<00:00:03.600><c> are</c><00:00:03.900><c> you</c>\n";
        let cues = parse_cues(input);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].end_ms, 3500);
        assert_eq!(cues[0].text, "good morning");
        assert_eq!(cues[1].text, "how are you");
    }

    #[test]
    fn parses_srt_with_comma_and_index_lines() {
        let input = "1\n00:00:01,500 --> 00:00:02,900\n你好 &amp; 世界\n\n2\n00:00:03,000 --> 00:00:04,000\n第二句\n";
        let cues = parse_cues(input);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1500);
        assert_eq!(cues[0].end_ms, 2900);
        assert_eq!(cues[0].text, "你好 & 世界");
        assert_eq!(cues[1].text, "第二句");
    }

    #[test]
    fn parses_short_timestamp_without_hours() {
        let input = "WEBVTT\n\n01:02.500 --> 01:04.000\n短格式\n";
        let cues = parse_cues(input);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 62_500);
        assert_eq!(cues[0].end_ms, 64_000);
    }

    #[test]
    fn merges_overlay_by_timeline_overlap_not_line_number() {
        let timeline = vec![
            SubtitleCue {
                start_ms: 0,
                end_ms: 2000,
                text: "第一句中文".into(),
            },
            SubtitleCue {
                start_ms: 2000,
                end_ms: 4000,
                text: "第二句中文".into(),
            },
        ];
        // 英文轨切得更碎：两条英文都落在第一句中文的时间范围内。
        let overlay = vec![
            SubtitleCue {
                start_ms: 0,
                end_ms: 900,
                text: "first".into(),
            },
            SubtitleCue {
                start_ms: 900,
                end_ms: 2000,
                text: "part two".into(),
            },
            SubtitleCue {
                start_ms: 2000,
                end_ms: 4000,
                text: "second".into(),
            },
        ];
        let merged = merge_overlay(&timeline, &overlay, Some("en"));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "part two\n第一句中文");
        assert_eq!(merged[1].text, "second\n第二句中文");
    }

    #[test]
    fn merge_keeps_timeline_text_when_overlay_missing() {
        let timeline = vec![SubtitleCue {
            start_ms: 0,
            end_ms: 1000,
            text: "只有中文".into(),
        }];
        let overlay = vec![SubtitleCue {
            start_ms: 5000,
            end_ms: 6000,
            text: "late english".into(),
        }];
        let merged = merge_overlay(&timeline, &overlay, Some("en"));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "只有中文");
    }

    /// ASS 渲染：头部完整、每条 cue 一行 Dialogue、时间戳为 `H:MM:SS.CS`。
    #[test]
    fn render_ass_emits_dialogue_lines_with_ass_timestamps() {
        use super::render_ass;

        let cues = vec![
            SubtitleCue {
                start_ms: 700,
                end_ms: 1_620,
                text: "哈喽大家好".to_string(),
            },
            SubtitleCue {
                start_ms: 3_675_000,
                end_ms: 3_678_500,
                text: "第一行\n第二行".to_string(),
            },
        ];
        let output = render_ass(&cues);
        assert!(output.starts_with("[Script Info]\n"));
        assert!(output.contains("[V4+ Styles]\n"));
        assert!(output.contains("[Events]\n"));
        assert!(output.contains("0:00:00.70,0:00:01.62,Default,,0,0,0,,哈喽大家好"));
        // 3675000ms = 1:01:15.00；小时位不补零，多行文本用 \N 连接
        assert!(output.contains("1:01:15.00,1:01:18.50,Default,,0,0,0,,第一行\\N第二行"));
        assert_eq!(output.matches("Dialogue: 0,").count(), 2);
    }

    /// ASS 里 `{}` 是 override 标签边界、`\` 是转义引导符：正文出现时必须转义，
    /// 否则播放器会把字幕内容当成样式指令吃掉。
    #[test]
    fn render_ass_escapes_override_braces_and_backslashes() {
        use super::render_ass;

        let cues = vec![SubtitleCue {
            start_ms: 0,
            end_ms: 1000,
            text: "{笑} 反斜杠 \\ 与 ${\\an8} 都要原样显示".to_string(),
        }];
        let output = render_ass(&cues);
        assert!(
            output.contains("\\{笑\\} 反斜杠 \\\\ 与 $\\{\\\\an8\\} 都要原样显示"),
            "正文花括号与反斜杠都要转义，实际: {}",
            output.lines().last().unwrap_or_default()
        );
    }

    #[test]
    fn renders_srt_with_index_lines_and_comma_timestamps() {
        let cues = vec![
            SubtitleCue {
                start_ms: 0,
                end_ms: 2000,
                text: "english\n中文".into(),
            },
            SubtitleCue {
                start_ms: 3_725_000,
                end_ms: 3_726_500,
                text: "第三行".into(),
            },
        ];
        let rendered = render_srt(&cues);
        assert!(rendered.starts_with("1\n00:00:00,000 --> 00:00:02,000\nenglish\n中文\n\n"));
        assert!(rendered.contains("2\n01:02:05,000 --> 01:02:06,500\n第三行\n"));
    }

    #[test]
    fn cleans_platform_emitted_cue_text() {
        assert_eq!(clean_cue_text("good<00:00:01.200><c> morning</c>"), "good morning");
        assert_eq!(clean_cue_text("  a &amp; b  "), "a & b");
        assert_eq!(clean_cue_text("   "), "");
    }

    #[test]
    fn parses_bilingual_language_pair() {
        assert_eq!(
            parse_bilingual_language_pair("zh-CN+en"),
            Some(("zh-CN".to_string(), "en".to_string()))
        );
        assert_eq!(
            parse_bilingual_language_pair(" ja-JP + ko-KR "),
            Some(("ja-JP".to_string(), "ko-KR".to_string()))
        );
        assert_eq!(parse_bilingual_language_pair("zh-CN"), None);
        assert_eq!(parse_bilingual_language_pair("zh-CN+"), None);
        assert_eq!(primary_language("zh-CN+en"), "zh-CN");
        assert_eq!(primary_language("zh-CN"), "zh-CN");
    }
}
