use std::fmt::Display;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::utils::subtitle::{
    bilingual_ratio, clean_cue_text, merge_overlay, parse_bilingual_language_pair, primary_language, render_srt,
    SubtitleCue,
};

/// `#[serde(default)]` 只处理「字段缺失」，不处理「字段为 null」。
/// B 站风控/降级时两类都会出现，用这个反序列化器把 null 也折叠成默认值。
fn deserialize_null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    T: Default + serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    let value = <Option<T> as serde::Deserialize>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct SubTitlesInfo {
    #[serde(deserialize_with = "deserialize_null_default")]
    pub subtitles: Vec<SubTitleInfo>,
}

// B 站风控/降级时 `subtitles` 里的元素可能缺字段或字段为 null。
// 缺省即可，避免一条坏轨让整份字幕列表反序列化失败。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SubTitleInfo {
    #[serde(deserialize_with = "deserialize_null_default")]
    pub lan: String,
    #[serde(deserialize_with = "deserialize_null_default")]
    pub subtitle_url: String,
}

pub struct SubTitle {
    pub lan: String,
    pub body: SubTitleBody,
    /// 是否为 AI 自动生成轨。`is_ai_sub()` 还会按 CDN URL 判定（`ai_subtitle`），
    /// 所以不能只看 `lan` 是否有 `ai-` 前缀，否则同语言的人工轨与 AI 轨会同名覆盖。
    pub is_ai: bool,
}

/// 字幕获取结果。除了已下载的轨，还要把「本轮结果是否不完整」回传给工作流，
/// 由工作流决定字幕状态位能否标记完成（否则补上 Cookie 后也不会再拉 AI 轨）。
pub struct SubtitleFetchOutcome {
    pub subtitles: Vec<SubTitle>,
    /// 接口声明需要登录、且本轮一条 AI 轨都没拿到：属于可恢复的「部分缺失」。
    pub pending_ai_subtitle: bool,
    /// 有条目在下载/解析阶段失败：本轮只拿到了部分轨，需要按可重试收尾，
    /// 否则一条 404 会让那条轨永久丢失。
    pub incomplete_tracks: bool,
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

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct SubTitleItem {
    #[serde(deserialize_with = "deserialize_null_default")]
    from: f64,
    #[serde(deserialize_with = "deserialize_null_default")]
    to: f64,
    #[serde(deserialize_with = "deserialize_null_default")]
    content: String,
}

/// 并发子任务的汇总结果：成功项 + 失败条数。
///
/// 保留成功项**不等于**把部分失败当成成功：调用方必须根据 `failed` 决定
/// 这一页是否按可重试收尾，否则一次 404 就会让那条轨永久丢失。
#[derive(Debug)]
pub(crate) struct PartialOutcome<T> {
    pub values: Vec<T>,
    /// 失败的子任务条数（成功项已放进 `values`）。
    pub failed: usize,
    /// 第一条失败的错误，用于拼提示文案。
    pub first_error: Option<anyhow::Error>,
}

impl<T> PartialOutcome<T> {
    pub fn is_partial(&self) -> bool {
        self.failed > 0
    }

    /// 第一条失败的简要原因，供日志与重试提示复用。
    pub fn first_error_summary(&self) -> String {
        self.first_error
            .as_ref()
            .map(|err| format!("{err:#}"))
            .unwrap_or_default()
    }
}

/// 汇总并发子任务（下载单条字幕轨、写入单个 sidecar 文件）的结果。
///
/// 旧实现用 `FuturesUnordered::try_collect()`，任意一条轨失败就会让整批结果作废，
/// 于是「一条坏轨 => 一个字幕文件都落不了盘」。这里改成保留成功项、逐条告警，
/// 只有全部失败时才把首个错误抛给上层，交给状态机做有限次重试；
/// 部分失败则由调用方用 `failed` 判断是否同样按可重试收尾。
pub(crate) fn keep_successful<T>(outcomes: Vec<Result<T>>) -> Result<PartialOutcome<T>> {
    let total = outcomes.len();
    let mut values = Vec::with_capacity(total);
    let mut failed = 0usize;
    let mut first_error: Option<anyhow::Error> = None;
    for (index, outcome) in outcomes.into_iter().enumerate() {
        match outcome {
            Ok(value) => values.push(value),
            Err(err) => {
                failed += 1;
                tracing::warn!("字幕子任务失败（第 {}/{} 条）: {:#}", index + 1, total, err);
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }
    if values.is_empty() {
        if let Some(err) = first_error {
            return Err(err);
        }
    }
    Ok(PartialOutcome {
        values,
        failed,
        first_error,
    })
}

impl SubTitleInfo {
    pub fn is_ai_sub(&self) -> bool {
        // ai： aisubtitle.hdslb.com/bfs/ai_subtitle/xxxx
        // 非 ai： aisubtitle.hdslb.com/bfs/subtitle/xxxx
        // 两个判据取并集：`lan` 前缀是接口语义（AI 轨一律以 `ai-` 开头），
        // URL 是历史实现依赖的存储路径，任一命中都视为 AI 轨，避免 CDN 路径调整后漏判。
        self.lan.trim().to_ascii_lowercase().starts_with("ai-") || self.subtitle_url.contains("ai_subtitle")
    }

    /// 把接口返回的 `subtitle_url` 归一化成可直接交给 `reqwest` 的绝对地址。
    ///
    /// B 站历史上返回协议相对地址（`//aisubtitle.hdslb.com/...`），但 CDN 调整后
    /// 也会直接返回绝对地址（`https://aisubtitle.hdslb.com/...`）。旧实现在下载处
    /// 硬编码 `format!("https:{}", url)`，绝对地址会被拼成 `https:https://...`。
    /// 这里作为**唯一**的归一化入口，选轨与下载共用，保证两者判定一致。
    pub fn normalized_url(&self) -> Option<String> {
        let url = self.subtitle_url.trim();
        if url.is_empty() {
            return None;
        }
        if let Some(host_and_path) = url.strip_prefix("//") {
            if host_and_path.is_empty() {
                return None;
            }
            return Some(format!("https://{host_and_path}"));
        }
        let lowered = url.to_ascii_lowercase();
        if lowered.starts_with("https://") || lowered.starts_with("http://") {
            return Some(url.to_string());
        }
        None
    }

    /// 是否拿到了可下载的地址。
    ///
    /// 接口偶发会整批返回空 `subtitle_url`（风控或降级），这种轨下载必然失败；
    /// 提前剔除，避免一条坏轨把整个字幕子任务拖成失败。
    pub fn is_downloadable(&self) -> bool {
        self.normalized_url().is_some()
    }
}

/// `need_login_subtitle=true` 时，B 站往往仍会返回人工字幕轨，但把 AI 轨藏起来。
/// 旧实现只在「一条轨都没有」时报错，于是「有人工轨、AI 轨静默缺失」被当成成功，
/// 表现为「只有部分视频有 AI 字幕」。这里把它识别成可恢复的部分缺失。
pub(crate) fn pending_ai_subtitle_for_login(
    login_required: bool,
    options: &SubtitleDownloadOptions,
    requested: &[SubTitleInfo],
) -> bool {
    login_required && options.download_ai_subtitle && !requested.iter().any(|info| info.is_ai_sub())
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
        // 旧实现用 `zh-CN-en` 这种「语言-语言」拼接，它不是合法的 BCP-47 标签，
        // Jellyfin/Emby 会把整段当成流标题、语言字段留空，自动选轨与字幕转码随之失效。
        // 改成 `<目标语言>.bilingual`：语言码可被识别，`bilingual` 作为流标题保留可读性。
        lan: format!("{}.bilingual", subtitle_file_tag(&primary_track.lan)),
        body: render_srt(&merged),
    })
}

/// 生成文件名里的语言后缀。
///
/// B 站 AI 轨的 `lan` 是 `ai-zh` 这类**内部标记**而非语言码：按 BCP-47，`ai`
/// 不是已注册的语言子标签，直接写进 `xxx.ai-zh.srt` 会被 Jellyfin/Emby 当成
/// 「标题为 ai-zh 的未知语言轨」。这里统一归一成媒体库可识别的标签。
pub(crate) fn subtitle_file_tag(language: &str) -> String {
    let normalized = normalize_language(language);
    if normalized == "zh-cn" {
        return "zh-CN".to_string();
    }
    // BCP-47 子标签只允许字母、数字与连字符；接口偶发返回带空格/下划线等脏值，
    // 原样写进文件名会让整段后缀退化成「标题」。
    let cleaned: String = normalized
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        // BCP-47 的「语言未定」标准标签，至少保证媒体库能解析出一条语言轨。
        "und".to_string()
    } else {
        cleaned.to_string()
    }
}

/// 由基础 `.srt` 路径与 B 站语言码生成单轨 sidecar 路径。
/// 单轨与双语合并产物都必须经过同一套语言标签归一，避免两条路径行为不一致。
///
/// AI 轨与人工轨可能同语言（`ai-zh` 与 `zh-CN` 都归一成 `zh-CN`），两者都要保留，
/// 因此 AI 轨追加 `.ai` 作为流标题：`xxx.zh-CN.ai.srt` 里 `zh-CN` 仍是合法的
/// BCP-47 语言标签，媒体库能正确识别语言，又不会与人工轨同名互相覆盖。
pub(crate) fn subtitle_sidecar_path(base: &Path, language: &str, is_ai: bool) -> PathBuf {
    let tag = subtitle_file_tag(language);
    let suffix = if is_ai || language.trim().to_ascii_lowercase().starts_with("ai-") {
        format!("{tag}.ai")
    } else {
        tag
    };
    base.with_extension(format!("{}.srt", suffix))
}

impl SubTitlesInfo {
    pub fn into_downloadable_subtitles(self, options: &SubtitleDownloadOptions) -> Vec<SubTitleInfo> {
        let mut seen_tracks = std::collections::HashSet::new();
        let mut selected = Vec::new();
        let mut ai_subtitles = Vec::new();

        // 去重键必须带上「轨类型」：人工轨 `zh-CN` 与 AI 轨 `ai-zh` 归一后都是
        // `zh-cn`，旧实现只按语言去重会把 AI 轨整条丢掉——开了 AI 字幕开关却
        // 拿不到 AI 轨。两条轨会按上面的规则落到不同文件名，不会互相覆盖。
        let track_key = |subtitle: &SubTitleInfo| {
            format!(
                "{}:{}",
                if subtitle.is_ai_sub() { "ai" } else { "manual" },
                normalize_language(&subtitle.lan)
            )
        };

        for subtitle in self.subtitles {
            if !subtitle.is_downloadable() {
                continue;
            }
            if subtitle.is_ai_sub() {
                ai_subtitles.push(subtitle);
            } else if seen_tracks.insert(track_key(&subtitle)) {
                selected.push(subtitle);
            }
        }

        if options.download_ai_subtitle {
            // B 站字幕接口只认单一语言码，双语设置（如 `zh-CN+en`）要先剥成目标语言。
            let requested_language = primary_language(&options.ai_subtitle_language);
            let selected_ai_subtitle = find_ai_subtitle_by_language(&ai_subtitles, requested_language)
                .or_else(|| find_ai_subtitle_by_language(&ai_subtitles, DEFAULT_AI_SUBTITLE_LANGUAGE));

            if let Some(subtitle) = selected_ai_subtitle {
                if seen_tracks.insert(track_key(&subtitle)) {
                    selected.push(subtitle);
                }
            }

            // 双语设置还要把对照语言的 AI 轨一并取回，否则上游只能拿到单语。
            if let Some((_, secondary)) = parse_bilingual_language_pair(&options.ai_subtitle_language) {
                if !language_matches(&secondary, requested_language) {
                    if let Some(subtitle) = find_ai_subtitle_by_language(&ai_subtitles, &secondary) {
                        if seen_tracks.insert(track_key(&subtitle)) {
                            selected.push(subtitle);
                        }
                    }
                }
            }
        }

        selected
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
    // `ai-` 是 B 站用来标记「AI 自动生成」的前缀，不属于语言码本身。
    // 先通用剥离，再走别名表：否则白名单外的 AI 轨（ai-es / ai-fr / ai-zh-Hant）
    // 既选不中，落盘文件名也会带上非法的 `ai` 子标签。
    let mut normalized = language.trim().to_ascii_lowercase();
    if let Some(rest) = normalized.strip_prefix("ai-") {
        normalized = rest.to_string();
    }
    match normalized.as_str() {
        "zh" | "zh-cn" | "zh-hans" | "cmn-hans" => "zh-cn".to_string(),
        "en" | "en-us" | "en-gb" => "en".to_string(),
        "ja" | "ja-jp" | "jp" => "ja".to_string(),
        "ko" | "ko-kr" | "kr" => "ko".to_string(),
        other => other.to_string(),
    }
}

impl Display for SubTitleBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 单轨与双语合并产物必须走同一套规则：1-based 序号、整数毫秒时间戳、
        // 内联标签清洗、按时间排序、丢弃空 cue。旧实现直接输出 `item.content`，
        // 导致同一份字幕在两条路径下产出不同的 SRT。
        f.write_str(&render_srt(&self.to_cues()))
    }
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
    fn srt_body_uses_rounded_integer_milliseconds() {
        // 旧实现用 `f64.fract() * 1e3`，206.45 会变成 ,449；统一走 render_srt 后
        // 全部按整数毫秒渲染。
        let testcases = [
            (0.0, 1.5, "00:00:00,000 --> 00:00:01,500"),
            (206.45, 207.0, "00:03:26,450 --> 00:03:27,000"),
            (360001.23, 360002.0, "100:00:01,230 --> 100:00:02,000"),
            (-1.0, 0.0, "00:00:00,000 --> 00:00:00,000"),
        ];
        for (from, to, expect) in testcases {
            let body = super::SubTitleBody(vec![item(from, to, "内容")]);
            assert!(body.to_string().contains(expect), "from={from}: {}", body);
        }
    }

    /// 回归：单轨路径曾直接输出 `item.content`，不清洗标签、不排序、不过滤空 cue，
    /// 与双语合并路径的产物规则不一致。
    #[test]
    fn single_track_srt_matches_merged_path_rules() {
        let body = super::SubTitleBody(vec![
            item(2.0, 3.0, "<c>后一句</c>"),
            item(0.0, 1.0, "前一句"),
            item(1.0, 2.0, "   "),
        ]);
        let rendered = body.to_string();
        assert_eq!(rendered, crate::utils::subtitle::render_srt(&body.to_cues()));
        assert!(rendered.starts_with("1\n00:00:00,000 --> 00:00:01,000\n前一句\n\n"));
        assert!(rendered.contains("2\n00:00:02,000 --> 00:00:03,000\n后一句\n\n"));
        assert!(!rendered.contains("<c>"), "内联标签应被清洗: {rendered}");
        assert_eq!(rendered.matches("-->").count(), 2, "空 cue 应被丢弃: {rendered}");
    }

    /// 回归：单轨 SRT 曾经从序号 0 开始，播放器会拒绝加载整份文件。
    #[test]
    fn srt_body_starts_at_index_one() {
        let body = super::SubTitleBody(vec![item(0.0, 1.5, "第一句"), item(1.5, 3.0, "第二句")]);
        assert_eq!(
            body.to_string(),
            "1\n00:00:00,000 --> 00:00:01,500\n第一句\n\n2\n00:00:01,500 --> 00:00:03,000\n第二句\n\n"
        );
    }

    /// 回归：单轨曾直接把 `lan` 写进文件名，生成 `xxx.ai-zh.srt`。
    /// `ai` 不是 BCP-47 语言子标签，媒体库只能把它当「标题为 ai-zh 的未知语言轨」，
    /// 按语言偏好自动选轨 / 字幕转码烧录随之失效。
    #[test]
    fn subtitle_file_tag_maps_ai_tracks_to_legal_language_tags() {
        let cases = [
            ("ai-zh", "zh-CN"),
            ("ai-zh-CN", "zh-CN"),
            ("zh-CN", "zh-CN"),
            ("zh-Hans", "zh-CN"),
            ("ai-en", "en"),
            ("ai-es", "es"),
            ("ai-fr", "fr"),
            ("ai-zh-Hant", "zh-hant"),
            ("ai-ja", "ja"),
        ];
        for (input, expected) in cases {
            assert_eq!(super::subtitle_file_tag(input), expected, "input={input}");
        }
        // 脏值不能让整段后缀退化成标题
        assert_eq!(super::subtitle_file_tag("zh CN!"), "zh-cn");
        assert_eq!(super::subtitle_file_tag(""), "und");
        assert!(!super::subtitle_file_tag("ai-zh").contains("ai"));
    }

    /// 单轨与双语产物必须共用同一套语言标签归一；AI 轨要额外加 `.ai` 区分，
    /// 避免与同语言的人工轨落盘到同一个文件互相覆盖。
    #[test]
    fn subtitle_sidecar_path_normalizes_ai_track_language() {
        let base = std::path::Path::new("/media/视频/第01集.srt");
        assert_eq!(
            super::subtitle_sidecar_path(base, "ai-zh", true),
            std::path::Path::new("/media/视频/第01集.zh-CN.ai.srt")
        );
        assert_eq!(
            super::subtitle_sidecar_path(base, "ai-en", true),
            std::path::Path::new("/media/视频/第01集.en.ai.srt")
        );
        assert_eq!(
            super::subtitle_sidecar_path(base, "zh-CN", false),
            std::path::Path::new("/media/视频/第01集.zh-CN.srt")
        );
        // URL 被判定为 AI 轨、但 lan 没有 ai- 前缀时，也必须靠 is_ai 区分。
        assert_eq!(
            super::subtitle_sidecar_path(base, "zh-CN", true),
            std::path::Path::new("/media/视频/第01集.zh-CN.ai.srt")
        );
    }

    /// 回归：人工轨 `zh-CN` 与 AI 轨 `ai-zh` 归一后语言相同，旧实现按语言去重
    /// 会把 AI 轨丢掉，导致「开启 AI 字幕」却拿不到 AI 轨。
    #[test]
    fn manual_and_ai_tracks_of_same_language_are_both_kept() {
        let info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                { "lan": "zh-CN", "subtitle_url": "//cdn.example.com/manual.json" },
                { "lan": "ai-zh", "subtitle_url": "//cdn.example.com/ai.json" }
            ]
        }))
        .unwrap();
        let tracks = info.into_downloadable_subtitles(&super::SubtitleDownloadOptions::default());
        assert_eq!(tracks.len(), 2, "人工轨与 AI 轨应同时保留: {tracks:?}");
        assert!(tracks.iter().any(|track| track.lan == "zh-CN"));
        assert!(tracks.iter().any(|track| track.lan == "ai-zh"));

        let base = std::path::Path::new("/media/第01集.srt");
        let paths: std::collections::HashSet<_> = tracks
            .iter()
            .map(|track| super::subtitle_sidecar_path(base, &track.lan, track.is_ai_sub()))
            .collect();
        assert_eq!(paths.len(), 2, "两条轨不能落到同一个文件: {paths:?}");
    }

    /// 白名单外的 AI 语言轨也应能被选中（旧实现会把 `ai-es` 一律回退成中文 AI 轨）。
    #[test]
    fn ai_track_prefix_is_stripped_for_any_language() {
        let info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                { "lan": "ai-zh", "subtitle_url": "//aisubtitle.hdslb.com/zh.json" },
                { "lan": "ai-es", "subtitle_url": "//aisubtitle.hdslb.com/es.json" }
            ]
        }))
        .unwrap();
        let subtitles = info.into_downloadable_subtitles(&super::SubtitleDownloadOptions {
            download_ai_subtitle: true,
            ai_subtitle_language: "es".to_string(),
        });
        assert_eq!(subtitles.len(), 1);
        assert_eq!(subtitles[0].lan, "ai-es");
    }

    /// 回归：`is_downloadable()` 放行绝对 URL，但下载处硬编码 `format!("https:{}", url)`。
    #[test]
    fn normalized_url_handles_protocol_relative_and_absolute_urls() {
        let make = |url: &str| super::SubTitleInfo {
            lan: "ai-zh".to_string(),
            subtitle_url: url.to_string(),
        };

        assert_eq!(
            make("//aisubtitle.hdslb.com/bfs/ai_subtitle/a.json")
                .normalized_url()
                .as_deref(),
            Some("https://aisubtitle.hdslb.com/bfs/ai_subtitle/a.json")
        );
        assert_eq!(
            make("  https://aisubtitle.hdslb.com/bfs/subtitle/a.json  ")
                .normalized_url()
                .as_deref(),
            Some("https://aisubtitle.hdslb.com/bfs/subtitle/a.json")
        );
        assert_eq!(
            make("http://aisubtitle.hdslb.com/a.json").normalized_url().as_deref(),
            Some("http://aisubtitle.hdslb.com/a.json")
        );
        for invalid in ["", "   ", "//", "aisubtitle.hdslb.com/a.json", "ftp://host/a.json"] {
            assert!(make(invalid).normalized_url().is_none(), "url={invalid}");
            assert!(!make(invalid).is_downloadable(), "url={invalid}");
        }
        for valid in [
            "//aisubtitle.hdslb.com/a.json",
            "https://aisubtitle.hdslb.com/a.json",
            "http://aisubtitle.hdslb.com/a.json",
        ] {
            assert!(make(valid).is_downloadable(), "url={valid}");
        }
    }

    /// 风控降级时 B 站可能返回缺字段或 null 的轨，不能让整份列表反序列化失败。
    #[test]
    fn subtitle_metadata_tolerates_missing_and_null_fields() {
        let info: super::SubTitlesInfo = serde_json::from_value(serde_json::json!({
            "subtitles": [
                { "lan": null, "subtitle_url": null },
                { "lan": "ai-zh" },
                { "lan": "zh-CN", "subtitle_url": "//aisubtitle.hdslb.com/a.json" }
            ]
        }))
        .unwrap();
        assert_eq!(info.subtitles.len(), 3);
        let usable = info.into_downloadable_subtitles(&super::SubtitleDownloadOptions::default());
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].lan, "zh-CN");

        let body: super::SubTitleBody = serde_json::from_value(serde_json::json!([
            { "from": 0.0, "to": 1.0, "content": "正常" },
            { "from": null, "to": 2.0 }
        ]))
        .unwrap();
        assert_eq!(body.0.len(), 2);
        assert!(body.to_string().contains("正常"));
    }

    /// 一条轨失败不应让其余轨一起作废，但必须把「有轨失败」如实报给调用方，
    /// 否则调用方会按成功收尾，那条轨就永久缺失了。
    #[test]
    fn keep_successful_keeps_partial_results() {
        let outcomes = vec![Ok(1), Err(anyhow::anyhow!("CDN 404")), Ok(3)];
        let partial = super::keep_successful(outcomes).unwrap();
        assert_eq!(partial.values, vec![1, 3]);
        assert_eq!(partial.failed, 1);
        assert!(partial.is_partial());
        assert_eq!(partial.first_error.unwrap().to_string(), "CDN 404");

        let all_ok = super::keep_successful(vec![Ok::<i32, anyhow::Error>(2)]).unwrap();
        assert_eq!(all_ok.values, vec![2]);
        assert!(!all_ok.is_partial());

        let all_failed = vec![
            Err::<i32, _>(anyhow::anyhow!("首个错误")),
            Err(anyhow::anyhow!("第二个错误")),
        ];
        assert_eq!(super::keep_successful(all_failed).unwrap_err().to_string(), "首个错误");

        let empty = super::keep_successful(Vec::<anyhow::Result<i32>>::new()).unwrap();
        assert!(empty.values.is_empty());
        assert_eq!(empty.failed, 0);
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

    /// 回归：同语言的人工轨与 AI 轨都要保留，且不能落到同一个文件。
    /// 这里 AI 轨由 CDN URL（`ai_subtitle`）判定，`lan` 仍是普通的 `zh-CN`；
    /// 旧实现只按语言去重会丢掉 AI 轨，且只看 `lan` 前缀生成文件名会互相覆盖。
    #[test]
    fn downloadable_subtitles_keep_manual_and_url_marked_ai_tracks() {
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

        assert_eq!(subtitles.len(), 2, "人工轨与 AI 轨应同时保留: {subtitles:?}");
        assert!(subtitles
            .iter()
            .any(|subtitle| subtitle.subtitle_url.contains("/subtitle/")));
        assert!(subtitles
            .iter()
            .any(|subtitle| subtitle.subtitle_url.contains("/ai_subtitle/")));
        let base = std::path::Path::new("/media/第01集.srt");
        let paths: std::collections::HashSet<_> = subtitles
            .iter()
            .map(|subtitle| super::subtitle_sidecar_path(base, &subtitle.lan, subtitle.is_ai_sub()))
            .collect();
        assert_eq!(paths.len(), 2, "两条轨不能落到同一个文件: {paths:?}");
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
            is_ai: false,
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

        assert_eq!(merged.lan, "zh-CN.bilingual");
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

        assert_eq!(merged.lan, "zh-CN.bilingual");
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
