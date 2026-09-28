//! 外源（抖音 / TikTok / YouTube）标题清洗。
//!
//! 平台标题里塞满了 `#话题` 标签、多行署名（曲编/歌手/混音…）、`回复 @某人 的评论`
//! 这类噪音，原样写进目录名会让文件夹又长又乱（例如
//! `怎么不是最权威的大合唱呢 #乐意效劳 #抖音音乐年终狂想 …`）。
//!
//! 这里生成的只是「用于命名文件/目录」的标题，视频信息（NFO）里仍然是平台原始标题，
//! 媒体库里看到的标题不会变。长度上限沿用与 B 站完全相同的机制：模板渲染后每个路径
//! 段最多 200 字节（见 [`crate::utils::filenamify`]）。

use once_cell::sync::OnceCell;
use regex::Regex;

/// `回复 @某人 的评论`，抖音会在标题里带上这条评论回复前缀。
static REPLY_NOISE: OnceCell<Regex> = OnceCell::new();
/// `#话题` / `＃话题`，直到下一个空格或下一个话题为止。
static HASHTAG: OnceCell<Regex> = OnceCell::new();
/// 孤立的 `#`（前后是空白或字符串边界），例如 `# 标题` 里剩下的井号。
static LONE_HASH: OnceCell<Regex> = OnceCell::new();

fn compiled(cell: &'static OnceCell<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("标题清洗正则不合法"))
}

/// 把标题清洗成适合做目录/文件名的形式。
///
/// 清洗规则：
/// 1. 只取第一行——多行标题后面通常是署名、简介，不属于标题本身；
/// 2. 去掉 `回复 @某人 的评论` 这类平台噪音；
/// 3. 去掉 `#话题` 标签（半角 `#` 与全角 `＃` 都算）；
/// 4. 去掉控制字符、零宽字符，把连续空白合成一个空格；
/// 5. 修剪首尾残留的分隔符（话题被删掉后常留下 `_`、`-`、`，` 等）；
/// 6. 清洗后为空时逐级回退（退到全部行、退到去掉井号的标签文字、最后退到原始标题），
///    保证目录名不为空。
///
/// 注意：这里不做长度截断，长度由模板渲染后的统一 200 字节/段限制负责。
pub fn clean_title_for_path(title: &str) -> String {
    let joined = title
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalize(&joined);

    // ① 首选第一行：多行标题的第一行才是标题本体。
    if let Some(first_line) = title.lines().map(str::trim).find(|line| !line.is_empty()) {
        let cleaned = strip_noise(first_line);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    // ② 第一行全部是话题标签（例如 `#tag\n真正的标题`）时退到整个标题。
    let cleaned = strip_noise(&normalized);
    if !cleaned.is_empty() {
        return cleaned;
    }

    // ③ 整个标题都是话题标签（例如 `#妃咲 #cos #蔚蓝档案`）时，留下标签文字、去掉井号，
    //    总比把一串 `#` 写进目录名好看。
    let without_hash_symbols = trim_separators(&normalize(&normalized.replace(['#', '＃'], " ")));
    if !without_hash_symbols.is_empty() {
        return without_hash_symbols;
    }

    // ④ 连标签文字都没有（例如 `###`）时保留原标题，避免生成空名字。
    if normalized.is_empty() {
        "unnamed".to_string()
    } else {
        normalized
    }
}

/// 去掉话题标签、回复前缀，并整理空白与首尾分隔符。
fn strip_noise(input: &str) -> String {
    let without_reply = compiled(&REPLY_NOISE, r"回复\s*@\S*?的评论").replace_all(input, " ");
    let without_hashtags = compiled(&HASHTAG, r"[#＃]+[^\s#＃]+").replace_all(&without_reply, " ");
    let without_lone_hash = compiled(&LONE_HASH, r"(^|\s)[#＃]+(\s|$)").replace_all(&without_hashtags, "$1");
    trim_separators(&normalize(&without_lone_hash))
}

/// 把各种空白统一成单个空格，并丢掉控制字符与零宽字符。
fn normalize(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut pending_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if is_invisible(ch) {
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(ch);
    }
    output
}

fn is_invisible(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}')
}

/// 话题标签被删掉后，标题首尾常常只剩标点，这里把它们清掉。
fn trim_separators(input: &str) -> String {
    input
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '_' | '-' | '·' | '|' | '｜' | ',' | '，' | '、' | ';' | '；' | ':' | '：' | '–' | '—'
                )
        })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_title_for_path;

    #[test]
    fn drops_hashtags_but_keeps_content() {
        assert_eq!(
            clean_title_for_path(
                "哇～Ed sheeran来杭州唱shape of you了 #Shapeofyou杀回来了 #一人一首Shapeofyou #edsheeran #shapeofyou #翻唱"
            ),
            "哇～Ed sheeran来杭州唱shape of you了"
        );
        // 话题标签紧贴正文、彼此不带空格也要能识别
        assert_eq!(
            clean_title_for_path("炮灰女配#青春 #AI创作浪潮计划 #即梦AI创作者成长计划"),
            "炮灰女配"
        );
    }

    #[test]
    fn keeps_only_first_line_when_credits_follow() {
        assert_eq!(
            clean_title_for_path(
                "【原神生日会】向着深空和群星 #2025原神生日会 #原神五周年 #原神空月之歌 #原神\n曲编：Soda纯白\n歌手：开心蛙蛙\n作词：举烛"
            ),
            "【原神生日会】向着深空和群星"
        );
    }

    #[test]
    fn drops_douyin_reply_prefix() {
        assert_eq!(
            clean_title_for_path("✨我将成为他们的第一点星光 回复 @将回忆拼好给你的评论 #莫宁 #鸣潮 #鸣潮莫宁"),
            "✨我将成为他们的第一点星光"
        );
    }

    #[test]
    fn trims_separator_left_over_from_removed_hashtags() {
        assert_eq!(
            clean_title_for_path("出镜：@奶油小米🧁 _#cos #正片 #cos正片"),
            "出镜：@奶油小米🧁"
        );
        assert_eq!(clean_title_for_path("标题 - #tag #tag2"), "标题");
    }

    #[test]
    fn falls_back_when_everything_is_a_hashtag() {
        // 整条标题只有话题标签时，留下标签文字、去掉井号，而不是生成空名字
        assert_eq!(clean_title_for_path("#翻唱"), "翻唱");
        assert_eq!(clean_title_for_path("#妃咲 #cos #蔚蓝档案"), "妃咲 cos 蔚蓝档案");
        // 第一行只有话题标签时，退到后面几行的正文
        assert_eq!(clean_title_for_path("#翻唱\n真的很好听"), "真的很好听");
        // 连标签文字都没有时保留原标题
        assert_eq!(clean_title_for_path("###"), "###");
    }

    #[test]
    fn keeps_plain_titles_untouched() {
        assert_eq!(clean_title_for_path("Hello World"), "Hello World");
        assert_eq!(
            clean_title_for_path("东方Project｜「蕾米莉亚·斯卡雷」。出镜：@奶油小米🧁"),
            "东方Project｜「蕾米莉亚·斯卡雷」。出镜：@奶油小米🧁"
        );
    }

    #[test]
    fn removes_control_and_zero_width_characters() {
        assert_eq!(clean_title_for_path("标题\u{200b}里的\u{0007}噪音"), "标题里的噪音");
    }
}
