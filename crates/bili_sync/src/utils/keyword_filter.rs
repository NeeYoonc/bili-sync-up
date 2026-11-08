//! 关键词过滤工具模块
//!
//! 提供视频标题的正则表达式关键词过滤功能
//! 支持黑名单和白名单两套列表同时生效：
//! - 白名单：如果设置了白名单，视频必须匹配其中之一才下载
//! - 黑名单：匹配黑名单的视频即使通过白名单也不下载
//!
//! 过滤逻辑：
//! 1. 如果有白名单 → 必须匹配白名单中的任一关键词
//! 2. 如果匹配黑名单 → 即使通过白名单也排除

use regex::{Regex, RegexBuilder};
use tracing::{debug, warn};

/// 关键词过滤模式（已废弃，保留用于向后兼容）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeywordFilterMode {
    /// 黑名单模式：匹配关键词的视频将被排除（不下载）
    #[default]
    Blacklist,
    /// 白名单模式：只下载匹配关键词的视频
    Whitelist,
}

impl KeywordFilterMode {
    /// 从字符串解析过滤模式
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "whitelist" => KeywordFilterMode::Whitelist,
            _ => KeywordFilterMode::Blacklist, // 默认为黑名单模式
        }
    }
}

/// 检查视频标题是否应该被过滤（使用独立的黑白名单列表）
///
/// 过滤逻辑：
/// 1. 如果有白名单且不为空 → 必须匹配白名单中的任一关键词，否则过滤
/// 2. 如果匹配黑名单中的任一关键词 → 过滤（即使通过了白名单）
///
/// # Arguments
/// * `title` - 视频标题
/// * `blacklist_keywords` - 黑名单关键词（JSON数组字符串）
/// * `whitelist_keywords` - 白名单关键词（JSON数组字符串）
/// * `case_sensitive` - 是否区分大小写（true=区分，false=不区分）
///
/// # Returns
/// * `true` - 视频应该被过滤（不下载）
/// * `false` - 视频不应该被过滤（可以下载）
pub fn should_filter_video_dual_list(
    title: &str,
    blacklist_keywords: &Option<String>,
    whitelist_keywords: &Option<String>,
    case_sensitive: bool,
) -> bool {
    let blacklist = blacklist_keywords
        .as_ref()
        .map(|s| parse_keywords(s, case_sensitive))
        .unwrap_or_default();
    let whitelist = whitelist_keywords
        .as_ref()
        .map(|s| parse_keywords(s, case_sensitive))
        .unwrap_or_default();

    // 1. 检查白名单（如果有白名单，必须匹配其中之一）
    if !whitelist.is_empty() {
        let matches_whitelist = whitelist.iter().any(|regex| regex.is_match(title));
        if !matches_whitelist {
            debug!("视频标题 '{}' 未匹配白名单关键词，将被过滤", title);
            return true; // 不在白名单中，过滤
        }
        debug!("视频标题 '{}' 匹配白名单关键词", title);
    }

    // 2. 检查黑名单（即使通过白名单，匹配黑名单的也过滤）
    if !blacklist.is_empty() {
        let matches_blacklist = blacklist.iter().any(|regex| regex.is_match(title));
        if matches_blacklist {
            debug!("视频标题 '{}' 匹配黑名单关键词，将被过滤", title);
            return true; // 在黑名单中，过滤
        }
    }

    // 通过所有检查，不过滤
    false
}

/// 检查视频标题是否应该被过滤（支持黑名单/白名单模式）
///
/// 已废弃：推荐使用 `should_filter_video_dual_list` 函数
///
/// # Arguments
/// * `title` - 视频标题
/// * `keyword_filters` - 关键词配置（JSON数组字符串）
/// * `filter_mode` - 过滤模式（黑名单/白名单）
///
/// # Returns
/// * `true` - 视频应该被过滤（不下载）
/// * `false` - 视频不应该被过滤（可以下载）
pub fn should_filter_video_with_mode(
    title: &str,
    keyword_filters: &Option<String>,
    filter_mode: &Option<String>,
) -> bool {
    let keywords = match keyword_filters {
        Some(filters) => parse_keywords(filters, true), // 旧API默认区分大小写
        None => return false,                           // 没有关键词配置，不过滤任何视频
    };

    if keywords.is_empty() {
        return false; // 关键词列表为空，不过滤任何视频
    }

    let mode = filter_mode
        .as_ref()
        .map(|m| KeywordFilterMode::from_str(m))
        .unwrap_or_default();

    let matches_any = keywords.iter().any(|regex| regex.is_match(title));

    match mode {
        KeywordFilterMode::Blacklist => {
            // 黑名单模式：匹配到关键词则过滤
            if matches_any {
                debug!("视频标题 '{}' 匹配黑名单关键词，将被过滤", title);
                true
            } else {
                false
            }
        }
        KeywordFilterMode::Whitelist => {
            // 白名单模式：没有匹配到关键词则过滤
            if matches_any {
                debug!("视频标题 '{}' 匹配白名单关键词，将被下载", title);
                false
            } else {
                debug!("视频标题 '{}' 未匹配白名单关键词，将被过滤", title);
                true
            }
        }
    }
}

/// 检查视频标题是否匹配任一关键词（正则表达式匹配）
///
/// 已废弃：推荐使用 `should_filter_video_dual_list` 函数
///
/// # Arguments
/// * `title` - 视频标题
/// * `keyword_filters` - 关键词配置（JSON数组字符串）
///
/// # Returns
/// * `true` - 匹配到任一关键词，应该被过滤
/// * `false` - 没有匹配任何关键词，不过滤
///
/// 解析关键词JSON数组为正则表达式列表
///
/// # Arguments
/// * `keyword_filters` - 关键词JSON数组字符串
/// * `case_sensitive` - 是否区分大小写
fn parse_keywords(keyword_filters: &str, case_sensitive: bool) -> Vec<Regex> {
    let keywords: Vec<String> = match serde_json::from_str(keyword_filters) {
        Ok(k) => k,
        Err(e) => {
            warn!("解析关键词过滤器失败: {}", e);
            return Vec::new();
        }
    };

    keywords
        .into_iter()
        .filter_map(
            |pattern| match RegexBuilder::new(&pattern).case_insensitive(!case_sensitive).build() {
                Ok(regex) => Some(regex),
                Err(e) => {
                    warn!("编译正则表达式 '{}' 失败: {}", pattern, e);
                    None
                }
            },
        )
        .collect()
}

/// 验证正则表达式是否有效
pub fn validate_regex(pattern: &str) -> Result<(), String> {
    match Regex::new(pattern) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("无效的正则表达式: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dual_list_no_filters() {
        // 没有任何过滤器，不过滤
        assert!(!should_filter_video_dual_list("测试视频", &None, &None, true));
        assert!(!should_filter_video_dual_list(
            "测试视频",
            &Some("[]".to_string()),
            &Some("[]".to_string()),
            true
        ));
    }

    #[test]
    fn test_dual_list_blacklist_only() {
        let blacklist = Some(r#"["广告", "预告"]"#.to_string());
        // 只有黑名单：匹配黑名单的被过滤
        assert!(should_filter_video_dual_list(
            "这是一个广告视频",
            &blacklist,
            &None,
            true
        ));
        assert!(should_filter_video_dual_list("预告片", &blacklist, &None, true));
        // 不匹配黑名单的不过滤
        assert!(!should_filter_video_dual_list("正常视频", &blacklist, &None, true));
    }

    #[test]
    fn test_dual_list_whitelist_only() {
        let whitelist = Some(r#"["PV", "MV"]"#.to_string());
        // 只有白名单：必须匹配白名单才下载
        assert!(!should_filter_video_dual_list("官方PV", &None, &whitelist, true));
        assert!(!should_filter_video_dual_list("新曲MV", &None, &whitelist, true));
        // 不匹配白名单的被过滤
        assert!(should_filter_video_dual_list("正常视频", &None, &whitelist, true));
        assert!(should_filter_video_dual_list("第一集", &None, &whitelist, true));
    }

    #[test]
    fn test_dual_list_both() {
        let blacklist = Some(r#"["预告"]"#.to_string());
        let whitelist = Some(r#"["PV"]"#.to_string());

        // 匹配白名单且不匹配黑名单 → 下载
        assert!(!should_filter_video_dual_list("官方PV", &blacklist, &whitelist, true));

        // 匹配白名单但也匹配黑名单 → 过滤（黑名单优先）
        assert!(should_filter_video_dual_list("预告PV", &blacklist, &whitelist, true));

        // 不匹配白名单 → 过滤
        assert!(should_filter_video_dual_list("第一集", &blacklist, &whitelist, true));

        // 不匹配白名单但匹配黑名单 → 过滤
        assert!(should_filter_video_dual_list("预告片", &blacklist, &whitelist, true));
    }

    #[test]
    fn test_dual_list_real_case() {
        // 你的例子：想下载PV但不想下载预告PV
        let blacklist = Some(r#"["预告"]"#.to_string());
        let whitelist = Some(r#"["PV"]"#.to_string());

        assert!(!should_filter_video_dual_list("XXX PV", &blacklist, &whitelist, true)); // 下载
        assert!(should_filter_video_dual_list("预告PV", &blacklist, &whitelist, true)); // 不下载
        assert!(should_filter_video_dual_list("第一集", &blacklist, &whitelist, true)); // 不下载
        assert!(should_filter_video_dual_list("预告", &blacklist, &whitelist, true));
        // 不下载
    }

    #[test]
    fn test_dual_list_case_sensitive() {
        // 测试大小写敏感
        let whitelist = Some(r#"["PV", "MV"]"#.to_string());

        // 区分大小写时，小写的 pv 不匹配
        assert!(should_filter_video_dual_list("官方pv", &None, &whitelist, true)); // 被过滤
        assert!(!should_filter_video_dual_list("官方PV", &None, &whitelist, true));
        // 不过滤
    }

    #[test]
    fn test_dual_list_case_insensitive() {
        // 测试大小写不敏感
        let whitelist = Some(r#"["PV", "MV"]"#.to_string());

        // 不区分大小写时，小写的 pv 也能匹配
        assert!(!should_filter_video_dual_list("官方pv", &None, &whitelist, false)); // 不过滤
        assert!(!should_filter_video_dual_list("官方PV", &None, &whitelist, false)); // 不过滤
        assert!(!should_filter_video_dual_list("官方Pv", &None, &whitelist, false));
        // 不过滤
    }

    #[test]
    fn test_blacklist_case_insensitive() {
        // 测试黑名单大小写不敏感
        let blacklist = Some(r#"["AD", "广告"]"#.to_string());

        // 不区分大小写时，小写的 ad 也能匹配黑名单
        assert!(should_filter_video_dual_list(
            "This is an ad video",
            &blacklist,
            &None,
            false
        )); // 被过滤
        assert!(should_filter_video_dual_list(
            "This is an AD video",
            &blacklist,
            &None,
            false
        )); // 被过滤

        // 区分大小写时，小写的 ad 不匹配
        assert!(!should_filter_video_dual_list(
            "This is an ad video",
            &blacklist,
            &None,
            true
        )); // 不过滤
        assert!(should_filter_video_dual_list(
            "This is an AD video",
            &blacklist,
            &None,
            true
        )); // 被过滤
    }

    #[test]
    fn test_validate_regex_valid() {
        assert!(validate_regex("测试").is_ok());
        assert!(validate_regex(r"\d+").is_ok());
        assert!(validate_regex(r"^prefix.*suffix$").is_ok());
    }

    #[test]
    fn test_validate_regex_invalid() {
        assert!(validate_regex(r"[").is_err());
        assert!(validate_regex(r"(unclosed").is_err());
    }
}
