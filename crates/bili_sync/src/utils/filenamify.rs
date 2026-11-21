use std::sync::atomic::{AtomicBool, Ordering};

macro_rules! regex {
    ($re:literal $(,)?) => {{
        static RE: once_cell::sync::OnceCell<regex::Regex> = once_cell::sync::OnceCell::new();
        RE.get_or_init(|| regex::Regex::new($re).expect("invalid regex"))
    }};
}

const MAX_FILENAME_BYTES: usize = 200;

/// 文件名“基名”（不含扩展名与附属后缀）的字节预算。
///
/// 多数文件系统（NAS 的 ext4/btrfs、Windows、SMB）对单个文件名（一个路径段）
/// 的限制是 255 字节，中文按 UTF-8 每字 3 字节计算。这里预留 20 字节给各种
/// 后缀，避免基名刚好卡线：`.zh-CN.default.ass`=18、`.mp4.tmp_video`=14、
/// `-fanart.jpg`=11、`.srt`=4。
pub const MAX_BASENAME_BYTES: usize = 235;
// 用 Unicode 私用区字符做占位符：这些字符不会出现在正常标题里，也不会被严格清洗误删。
const UNIX_SEP_PLACEHOLDER: &str = "\u{E000}UNIX_SEP_PROTECTED\u{E000}";
const WIN_SEP_PLACEHOLDER: &str = "\u{E000}WIN_SEP_PROTECTED\u{E000}";

/// 是否启用「严格清洗」：去掉 emoji、颜文字组合符等特殊字符。
static STRICT_CLEANING: AtomicBool = AtomicBool::new(true);

/// 设置是否启用文件名严格清洗。
pub fn set_strict_cleaning_enabled(enabled: bool) {
    STRICT_CLEANING.store(enabled, Ordering::Relaxed);
}

/// 当前是否启用严格清洗。
pub fn strict_cleaning_enabled() -> bool {
    STRICT_CLEANING.load(Ordering::Relaxed)
}

pub fn filenamify<S: AsRef<str>>(input: S) -> String {
    filenamify_with_options(input, false)
}

/// 按 UTF-8 字节数截断字符串（保证不会截断字符），并清理截断后残留的
/// 尾部分隔符/标点（空格、下划线、连字符、全角竖线、句点）。
pub fn truncate_utf8_bytes(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }

    let mut end = 0;
    for (idx, ch) in input.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }

    replace_outer_periods(input[..end].trim_matches(|c| c == ' ' || c == '_' || c == '-' || c == '｜'))
}

fn replace_outer_periods(input: &str) -> String {
    let outer_periods = regex!("^\\.+|\\.+$");
    outer_periods.replace_all(input, "_").into_owned()
}

fn truncate_filename_segment(segment: &str) -> String {
    let truncated = truncate_utf8_bytes(segment, MAX_FILENAME_BYTES);
    if truncated.is_empty() {
        "unnamed".to_string()
    } else {
        truncated
    }
}

fn truncate_filename_segments(input: &str, preserve_template_separators: bool) -> String {
    if !preserve_template_separators {
        return truncate_filename_segment(input);
    }

    let mut result = String::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        let unix_idx = remaining.find(UNIX_SEP_PLACEHOLDER);
        let win_idx = remaining.find(WIN_SEP_PLACEHOLDER);
        let next_sep = match (unix_idx, win_idx) {
            (Some(u), Some(w)) => Some((
                u.min(w),
                if u <= w {
                    UNIX_SEP_PLACEHOLDER
                } else {
                    WIN_SEP_PLACEHOLDER
                },
            )),
            (Some(u), None) => Some((u, UNIX_SEP_PLACEHOLDER)),
            (None, Some(w)) => Some((w, WIN_SEP_PLACEHOLDER)),
            (None, None) => None,
        };

        if let Some((idx, sep)) = next_sep {
            result.push_str(&truncate_filename_segment(&remaining[..idx]));
            result.push_str(sep);
            remaining = &remaining[idx + sep.len()..];
        } else {
            result.push_str(&truncate_filename_segment(remaining));
            break;
        }
    }

    result
}

/// 对可能包含路径分隔符的名称逐段做字节级限长（智能截断兜底）。
///
/// - 按 `/`、`\` 分段，每段独立限制在 `max_bytes` 字节内；
/// - 只在 UTF-8 字符边界截断，不会把中文/emoji 截坏；
/// - 保留原有分隔符与目录结构（空段原样保留）；
/// - 截断后清理段尾残留的空格/下划线/连字符等噪声。
///
/// 用于给“由多个变量拼接出来”的最终文件名兜底：此前各变量分别限长，
/// 但拼接后的整体仍可能超过文件系统单段 255 字节上限，导致下载器
/// 建临时文件时报 `Filename too long (os error 36)`。
pub fn cap_path_segments_bytes(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }

    let mut result = String::with_capacity(input.len());
    let mut segment_start = 0usize;
    for (idx, ch) in input.char_indices() {
        if ch == '/' || ch == '\\' {
            result.push_str(&truncate_segment_bytes(&input[segment_start..idx], max_bytes));
            result.push(ch);
            segment_start = idx + ch.len_utf8();
        }
    }
    result.push_str(&truncate_segment_bytes(&input[segment_start..], max_bytes));
    result
}

fn truncate_segment_bytes(segment: &str, max_bytes: usize) -> String {
    if segment.len() <= max_bytes {
        segment.to_string()
    } else {
        truncate_utf8_bytes(segment, max_bytes)
    }
}

/// 判断字符是否属于严格清洗要删除的类别：emoji（Extended_Pictographic 的主要区间）、
/// 变体选择符/零宽连接符，以及颜文字常用的组合符（Mn）与间隔修饰符（Sk）的常见区间。
fn is_strict_cleaning_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        // emoji / 图形符号
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
            // 变体选择符、零宽连接符
            | 0xFE0E
            | 0xFE0F
            | 0x200D
            // 颜文字常用的组合符（Mn）
            | 0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x0F71..=0x0F84
            | 0x0F86..=0x0F87
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
            // 颜文字常用的间隔修饰符（Sk）与其它修饰字母
            | 0x02B0..=0x02FF
            | 0x00A8
            | 0x00AF
            | 0x00B4
            | 0x00B8
    )
}

/// 删除 emoji、颜文字组合符等特殊字符。仅在真的删除了字符时重新整理分隔符，
/// 以保证没有特殊字符的既有文件名输出完全不变。
fn strict_clean_special_characters(input: &str) -> String {
    if !input.chars().any(is_strict_cleaning_char) {
        return input.to_string();
    }
    let mut cleaned = String::with_capacity(input.len());
    for ch in input.chars() {
        if is_strict_cleaning_char(ch) {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    let collapsed = regex!("_{2,}").replace_all(&cleaned, "_").into_owned();
    collapsed.trim_matches(|c| c == '_' || c == ' ').to_string()
}

/// 带选项的文件名安全化函数
///
/// # 参数
/// - `input`: 输入字符串
/// - `preserve_template_separators`: 是否保护模板路径分隔符（__UNIX_SEP__, __WIN_SEP__）
pub fn filenamify_with_options<S: AsRef<str>>(input: S, preserve_template_separators: bool) -> String {
    let mut input = input.as_ref().to_string();

    // 保护路径分隔符标记，避免被处理
    if preserve_template_separators {
        input = input.replace("__UNIX_SEP__", UNIX_SEP_PLACEHOLDER);
        input = input.replace("__WIN_SEP__", WIN_SEP_PLACEHOLDER);
    }

    // Windows不允许的字符：< > : " / \ | ? *
    // Unicode控制字符：\u{0000}-\u{001F} \u{007F} \u{0080}-\u{009F}
    let reserved = regex!("[<>:\"/\\\\|?*\u{0000}-\u{001F}\u{007F}\u{0080}-\u{009F}]+");

    // Windows保留名称：CON, PRN, AUX, NUL, COM1-COM9, LPT1-LPT9（不区分大小写）
    let windows_reserved = regex!("^(con|prn|aux|nul|com\\d|lpt\\d)$");

    // 全角字符映射
    let fullwidth_colon = regex!("："); // 全角冒号 → 半角冒号
    let fullwidth_space = regex!("　"); // 全角空格 → 半角空格
                                        // 其他可能有问题的字符（保留中文括号/书名号，避免过度清洗）
    let problematic_chars = regex!("[★☆♪♫♬♩♭♮♯※‖§¶°±×÷≈≠≤≥∞∴∵∠⊥∥∧∨∩∪⊂⊃⊆⊇∈∉∃∀]");

    let replacement = "_";
    let space_replacement = " ";
    let paren_replacement_left = "(";
    let paren_replacement_right = ")";
    let colon_replacement = "-";

    // 1. 处理全角字符映射
    input = fullwidth_colon.replace_all(&input, colon_replacement).into_owned();
    input = fullwidth_space.replace_all(&input, space_replacement).into_owned();
    // 2. 处理全角括号
    input = input.replace('（', paren_replacement_left);
    input = input.replace('）', paren_replacement_right);

    // 3. 处理其他有问题的字符
    input = problematic_chars.replace_all(&input, replacement).into_owned();

    // 4. 处理Windows保留字符
    input = reserved.replace_all(&input, replacement).into_owned();

    // 5. 处理开头和结尾的点号
    input = replace_outer_periods(&input);

    // 6. 检查Windows保留名称
    if windows_reserved.is_match(&input.to_lowercase()) {
        input.push_str(replacement);
    }

    // 7. 去除多余的连续下划线和空格，但保留某些特殊情况
    let cleanup_spaces = regex!(" {2,}"); // 多个连续空格 → 单个空格
    let cleanup_mixed = regex!("[_ ]{3,}"); // 混合的空格和下划线（3个或以上）→ 单个下划线
    let cleanup_underscores = regex!("_{3,}"); // 3个或以上连续下划线 → 双下划线

    // 清理空格和混合字符
    input = cleanup_spaces.replace_all(&input, " ").into_owned();
    input = cleanup_mixed.replace_all(&input, "_").into_owned();
    // 保留双下划线的特殊含义，但清理过多的连续下划线
    input = cleanup_underscores.replace_all(&input, "__").into_owned();

    // 8. 只去除开头和结尾的空格
    input = input.trim().to_string();

    // 8.5 严格清洗：去掉 emoji、颜文字组合符等特殊字符（默认开启，可在设置里关闭）
    if strict_cleaning_enabled() {
        input = strict_clean_special_characters(&input);
    }

    // 9. 确保文件名不为空
    if input.is_empty() {
        input = "unnamed".to_string();
    }

    // 10. 限制默认名称长度。按 UTF-8 字节截断，避免中文标题加 sidecar 后缀后超过文件系统限制。
    input = truncate_filename_segments(&input, preserve_template_separators);

    // 11. 恢复路径分隔符占位符（仅在保护模式下）
    if preserve_template_separators {
        input = input.replace(UNIX_SEP_PLACEHOLDER, "__UNIX_SEP__");
        input = input.replace(WIN_SEP_PLACEHOLDER, "__WIN_SEP__");
    }

    input
}

#[cfg(test)]
mod tests {
    use super::{filenamify, filenamify_with_options};

    /// 串行化会修改全局严格清洗开关的用例。
    static STRICT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_filenamify() {
        assert_eq!(filenamify("foo/bar"), "foo_bar");
        assert_eq!(filenamify("foo//bar"), "foo_bar");
        assert_eq!(filenamify("//foo//bar//"), "_foo_bar_");
        assert_eq!(filenamify("foo\\bar"), "foo_bar");
        assert_eq!(filenamify("foo\\\\\\bar"), "foo_bar");
    }

    #[test]
    fn test_filenamify_with_template_separators() {
        // 测试保护模板分隔符时，内容中的原始斜杠应该被处理
        let input = "ZHY2020__UNIX_SEP__【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身/ドッペルゲンガー」";
        let result = filenamify_with_options(input, true);

        // 期望结果：模板分隔符保留，但内容中的斜杠被处理
        assert_eq!(result, "ZHY2020__UNIX_SEP__【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身_ドッペルゲンガー」");
    }

    #[test]
    fn test_slash_in_content() {
        // 专门测试内容中的斜杠处理
        let input = "分身/ドッペルゲンガー";
        let result = filenamify(input);
        assert_eq!(result, "分身_ドッペルゲンガー");
    }

    #[test]
    fn test_filenamify_extended() {
        assert_eq!(filenamify(r"foo\\bar"), "foo_bar");
        assert_eq!(filenamify(r"foo\\\\\\bar"), "foo_bar");
        assert_eq!(filenamify("////foo////bar////"), "_foo_bar_");
        assert_eq!(filenamify("foo\u{0000}bar"), "foo_bar");
        assert_eq!(filenamify("\"foo<>bar*"), "_foo_bar_");
        assert_eq!(filenamify("."), "_");
        assert_eq!(filenamify(".."), "_");
        assert_eq!(filenamify("./"), "__");
        assert_eq!(filenamify("../"), "__");
        assert_eq!(filenamify("../../foo/bar"), "__.._foo_bar");
        assert_eq!(filenamify("foo.bar."), "foo.bar_");
        assert_eq!(filenamify("foo.bar.."), "foo.bar_");
        assert_eq!(filenamify("foo.bar..."), "foo.bar_");
        assert_eq!(filenamify("con"), "con_");
        assert_eq!(filenamify("com1"), "com1_");
        assert_eq!(filenamify(":nul|"), "_nul_");
        assert_eq!(filenamify("foo/bar/nul"), "foo_bar_nul");
        assert_eq!(filenamify("file:///file.tar.gz"), "file_file.tar.gz");
        assert_eq!(filenamify("http://www.google.com"), "http_www.google.com");
        assert_eq!(
            filenamify("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "https_www.youtube.com_watch_v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn test_filenamify_with_options() {
        // 测试保护模板分隔符
        assert_eq!(
            filenamify_with_options("foo__UNIX_SEP__bar", true),
            "foo__UNIX_SEP__bar"
        );
        assert_eq!(filenamify_with_options("foo__WIN_SEP__bar", true), "foo__WIN_SEP__bar");

        // 测试不保护模板分隔符时的行为
        assert_eq!(
            filenamify_with_options("foo__UNIX_SEP__bar", false),
            "foo__UNIX_SEP__bar" // 不包含真实分隔符，所以不受影响
        );

        // 测试用户问题中的场景：标题中包含分隔符
        assert_eq!(
            filenamify_with_options("【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身/ドッペルゲンガー」", false),
            "【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身_ドッペルゲンガー」"
        );

        // 测试模板和内容的组合情况
        assert_eq!(
            filenamify_with_options("UP主名__UNIX_SEP__「分身/ドッペルゲンガー」", true),
            "UP主名__UNIX_SEP__「分身_ドッペルゲンガー」"
        );
    }

    #[test]
    fn test_preserve_cjk_brackets_and_quotes() {
        assert_eq!(
            filenamify("〖周深｜MV〗《异人之下之决战！碧游村》主题曲《冰凌花》MV正式上线！"),
            "〖周深｜MV〗《异人之下之决战！碧游村》主题曲《冰凌花》MV正式上线！"
        );
        assert_eq!(
            filenamify("【合集】『标题』〔测试〕〈特别篇〉"),
            "【合集】『标题』〔测试〕〈特别篇〉"
        );
    }

    #[test]
    fn test_long_cjk_title_truncates_by_utf8_bytes() {
        let title = "变天了，请注意盖被子，我帮你盖的时候要记得说“谢谢你，宝宝”而不是“我草，你是怎么进来的？！”这种不知感恩的话，不要问我为什么会在你家里，我会一直看着你的，永远";
        let result = filenamify(title);

        assert!(result.len() <= super::MAX_FILENAME_BYTES);
        assert!(result.is_char_boundary(result.len()));
        assert!(result.starts_with("变天了，请注意盖被子"));
        assert!(result.len() < title.len());
    }

    #[test]
    fn test_path_segments_truncate_independently() {
        let long_title = "变天了，请注意盖被子，我帮你盖的时候要记得说“谢谢你，宝宝”而不是“我草，你是怎么进来的？！”这种不知感恩的话，不要问我为什么会在你家里，我会一直看着你的，永远";
        let result = filenamify_with_options(format!("UP主名__UNIX_SEP__{}", long_title), true);
        let parts = result.split("__UNIX_SEP__").collect::<Vec<_>>();

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "UP主名");
        assert!(parts[1].len() <= super::MAX_FILENAME_BYTES);
        assert!(parts[1].is_char_boundary(parts[1].len()));
    }

    #[test]
    fn test_truncated_name_does_not_end_with_period() {
        let title = format!("{}..tail", "a".repeat(super::MAX_FILENAME_BYTES - 2));
        let result = filenamify(title);

        assert!(result.len() <= super::MAX_FILENAME_BYTES);
        assert!(!result.ends_with('.'));
    }

    #[test]
    fn test_truncated_path_segment_does_not_end_with_period() {
        let title = format!("{}..tail", "a".repeat(super::MAX_FILENAME_BYTES - 2));
        let result = filenamify_with_options(format!("UP主名__UNIX_SEP__{}", title), true);
        let parts = result.split("__UNIX_SEP__").collect::<Vec<_>>();

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "UP主名");
        assert!(parts[1].len() <= super::MAX_FILENAME_BYTES);
        assert!(!parts[1].ends_with('.'));
    }

    #[test]
    fn test_cap_path_segments_bytes_truncates_long_compound_name() {
        // 模拟合集场景：视频标题 + 分P标题都很长的中文名拼接出 300+ 字节
        let video_title = "【一小时高效英语听力磨耳朵】发音+听力飞速提升｜从听不懂到秒懂".repeat(3);
        let page_title = "懶人必備_幫你搞定餐廳英語｜從點餐到結帳，最常用對話".repeat(3);
        let name = format!("S339E005 - {} - {}", video_title, page_title);
        assert!(name.len() > 255);

        let capped = super::cap_path_segments_bytes(&name, super::MAX_BASENAME_BYTES);

        assert!(capped.len() <= super::MAX_BASENAME_BYTES);
        assert!(capped.is_char_boundary(capped.len()));
        assert!(capped.starts_with("S339E005 - "));
        // 加上最长的附属后缀后仍在文件系统单段 255 字节上限内
        assert!(capped.len() + ".zh-CN.default.ass".len() <= 255);
        assert!(capped.len() + ".mp4.tmp_video".len() <= 255);
    }

    #[test]
    fn test_cap_path_segments_bytes_keeps_directory_structure() {
        let long = "长".repeat(300); // 900 字节
        let input = format!("{}/sub/{}", long, long);

        let capped = super::cap_path_segments_bytes(&input, 120);
        let parts: Vec<&str> = capped.split('/').collect();

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1], "sub");
        assert!(parts[0].len() <= 120);
        assert!(parts[2].len() <= 120);
        assert!(parts[0].is_char_boundary(parts[0].len()));
        assert!(parts[2].is_char_boundary(parts[2].len()));
    }

    #[test]
    fn test_truncate_utf8_bytes_cuts_on_char_boundary() {
        assert_eq!(super::truncate_utf8_bytes("中文标题abc", 7), "中文");
        assert_eq!(super::truncate_utf8_bytes("abc", 7), "abc");
        // 截断后残留的连字符会被清理，避免出现 "标题 - " 这类尾巴
        assert_eq!(super::truncate_utf8_bytes("中文标题 - 很长的标题", 14), "中文标题");
    }

    #[test]
    fn test_cap_path_segments_bytes_on_real_problem_title() {
        // 来自用户现场日志的真实失败案例：拼接后 348 字节，导致所有直链报
        // `Filename too long (os error 36)`（S339E006 - 视频标题 - 分P标题）。
        let video_title = "【一小时高效英语听力磨耳朵】发音+听力飞速提升｜从听不懂到秒懂，让英文听力爆发式成长";
        let page_title = "懶人必備_搞懂超市英語｜從選購到結帳｜最常用對話＋地道表達｜搞定購物、問價、結帳、退換貨｜英語磨耳朵｜無痛學習聽懂｜零基礎也能學會｜English Listening Practice";
        let raw = format!("S339E006 - {} - {}", filenamify(video_title), filenamify(page_title));
        assert!(raw.len() > 255, "原始拼接名应当超过 255 字节");

        let capped = super::cap_path_segments_bytes(&raw, super::MAX_BASENAME_BYTES);

        assert!(capped.len() <= super::MAX_BASENAME_BYTES);
        assert!(capped.is_char_boundary(capped.len()));
        assert!(capped.len() + ".mp4.tmp_video".len() <= 255);
        assert!(capped.starts_with("S339E006 - "));
        assert!(capped.contains("【一小时高效英语听力磨耳朵】"));
    }

    #[test]
    fn test_strict_cleaning_removes_emoji() {
        let _guard = STRICT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let cleaned = filenamify("测试🎃👻⭐标题❤️");
        assert!(!cleaned.chars().any(super::is_strict_cleaning_char));
        assert!(cleaned.contains("测试"));
        assert!(cleaned.contains("标题"));
    }

    #[test]
    fn test_strict_cleaning_removes_kaomoji_modifiers() {
        let _guard = STRICT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let cleaned = filenamify("(•̀ω•́)✧");
        assert!(!cleaned.chars().any(super::is_strict_cleaning_char));
        assert!(cleaned.contains('ω'));
        assert!(!cleaned.starts_with('_'));
        assert!(!cleaned.ends_with('_'));
    }

    #[test]
    fn test_strict_cleaning_can_be_disabled() {
        let _guard = STRICT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        super::set_strict_cleaning_enabled(false);
        let cleaned = filenamify("测试🎃标题");
        super::set_strict_cleaning_enabled(true);
        assert!(cleaned.contains('🎃'));
    }

    #[test]
    fn test_strict_cleaning_keeps_template_separators() {
        let _guard = STRICT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let cleaned = filenamify_with_options("A__UNIX_SEP__测试🎃B", true);
        assert!(cleaned.contains("__UNIX_SEP__"));
        assert!(!cleaned.chars().any(super::is_strict_cleaning_char));
    }

    #[test]
    fn test_strict_cleaning_falls_back_to_unnamed() {
        let _guard = STRICT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(filenamify("🎃👻"), "unnamed");
    }

    #[test]
    fn test_strict_cleaning_keeps_normal_titles_unchanged() {
        let _guard = STRICT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(filenamify("【4K修复】中文标题 ABC 123"), "【4K修复】中文标题 ABC 123");
        assert_eq!(filenamify("_foo_bar_"), "_foo_bar_");
    }
}
