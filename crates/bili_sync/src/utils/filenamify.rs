macro_rules! regex {
    ($re:literal $(,)?) => {{
        static RE: once_cell::sync::OnceCell<regex::Regex> = once_cell::sync::OnceCell::new();
        RE.get_or_init(|| regex::Regex::new($re).expect("invalid regex"))
    }};
}

pub fn filenamify<S: AsRef<str>>(input: S) -> String {
    filenamify_with_options(input, false)
}

/// 带选项的文件名安全化函数
///
/// # 参数
/// - `input`: 输入字符串
/// - `preserve_template_separators`: 是否保护模板路径分隔符（__UNIX_SEP__, __WIN_SEP__）
pub fn filenamify_with_options<S: AsRef<str>>(input: S, preserve_template_separators: bool) -> String {
    let mut input = input.as_ref().to_string();

    // 保护路径分隔符标记，避免被处理
    let unix_sep_placeholder = "🔒UNIX_SEP_PROTECTED🔒";
    let win_sep_placeholder = "🔒WIN_SEP_PROTECTED🔒";

    if preserve_template_separators {
        input = input.replace("__UNIX_SEP__", unix_sep_placeholder);
        input = input.replace("__WIN_SEP__", win_sep_placeholder);
    }

    // Windows不允许的字符：< > : " / \ | ? *
    // Unicode控制字符：\u{0000}-\u{001F} \u{007F} \u{0080}-\u{009F}
    let reserved = regex!("[<>:\"/\\\\|?*\u{0000}-\u{001F}\u{007F}\u{0080}-\u{009F}]+");

    // Windows保留名称：CON, PRN, AUX, NUL, COM1-COM9, LPT1-LPT9（不区分大小写）
    let windows_reserved = regex!("^(con|prn|aux|nul|com\\d|lpt\\d)$");

    // 文件名开头和结尾不能是点号
    let outer_periods = regex!("^\\.+|\\.+$");

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
    input = outer_periods.replace_all(&input, replacement).into_owned();

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

    // 9. 确保文件名不为空
    if input.is_empty() {
        input = "unnamed".to_string();
    }

    // 10. 限制文件名长度（Windows文件名最大255字符）
    if input.len() > 200 {
        input = input.chars().take(200).collect::<String>();
        // 确保不在多字节字符中间截断
        while !input.is_char_boundary(input.len()) {
            input.pop();
        }
        input = input.trim_matches(|c| c == ' ' || c == '_').to_string();
    }

    // 11. 恢复路径分隔符占位符（仅在保护模式下）
    if preserve_template_separators {
        input = input.replace(unix_sep_placeholder, "__UNIX_SEP__");
        input = input.replace(win_sep_placeholder, "__WIN_SEP__");
    }

    input
}

#[cfg(test)]
mod tests {
    use super::{filenamify, filenamify_with_options};

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
        assert_eq!(filenamify("【合集】『标题』〔测试〕〈特别篇〉"), "【合集】『标题』〔测试〕〈特别篇〉");
    }
}
