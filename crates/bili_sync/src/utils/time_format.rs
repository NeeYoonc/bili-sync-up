//! 统一的时间格式处理模块
//!
//! 本模块提供统一的时间格式化和解析功能
//! 标准格式：YYYY-MM-DD HH:MM:SS (不含毫秒和时区)

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Timelike, Utc};

/// 标准时间格式
pub const STANDARD_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// 北京时区 (UTC+8)
pub const BEIJING_OFFSET: i32 = 8 * 3600;

/// 获取北京时区
pub fn beijing_timezone() -> FixedOffset {
    FixedOffset::east_opt(BEIJING_OFFSET).unwrap()
}

/// 获取当前北京时间
pub fn beijing_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&beijing_timezone())
}

/// 获取当前时间的标准格式字符串（北京时间）
pub fn now_standard_string() -> String {
    now_naive().format(STANDARD_TIME_FORMAT).to_string()
}

/// 获取当前时间的 NaiveDateTime (北京时间，无时区信息)
pub fn now_naive() -> NaiveDateTime {
    // 获取北京时间，去除微秒部分
    let now = beijing_now();
    let truncated = now.with_nanosecond(0).unwrap_or(now);
    truncated.naive_local()
}

/// 将任意时间转换为标准格式字符串
pub fn to_standard_string<Tz: TimeZone>(dt: DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    dt.format(STANDARD_TIME_FORMAT).to_string()
}

/// 解析时间字符串，支持多种格式
pub fn parse_time_string(time_str: &str) -> Option<NaiveDateTime> {
    // 尝试多种格式解析

    // 1. 标准格式: YYYY-MM-DD HH:MM:SS
    if let Ok(dt) = NaiveDateTime::parse_from_str(time_str, STANDARD_TIME_FORMAT) {
        return Some(dt);
    }

    // 2. 带毫秒格式: YYYY-MM-DD HH:MM:SS.ffffff
    if let Ok(dt) = NaiveDateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S%.f") {
        return Some(dt);
    }

    // 3. 带时区格式: YYYY-MM-DD HH:MM:SS.ffffff +08:00
    if let Ok(dt) = DateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S%.f %z") {
        return Some(dt.naive_local());
    }

    // 4. 带时区但无毫秒: YYYY-MM-DD HH:MM:SS +08:00
    if let Ok(dt) = DateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S %z") {
        return Some(dt.naive_local());
    }

    // 5. RFC3339格式: YYYY-MM-DDTHH:MM:SS.ssssss+08:00
    if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
        return Some(dt.naive_local());
    }

    // 6. ISO8601格式变体
    if let Ok(dt) = time_str.parse::<DateTime<Utc>>() {
        return Some(dt.naive_local());
    }

    None
}

/// 将数据库中存储的北京时间 NaiveDateTime 转为 UTC。
pub fn stored_beijing_naive_to_utc(value: NaiveDateTime) -> DateTime<Utc> {
    beijing_timezone()
        .from_local_datetime(&value)
        .single()
        .expect("固定北京时间偏移应始终能映射本地时间")
        .with_timezone(&Utc)
}

/// 将 Unix 时间戳转换为标准格式字符串（北京时间）
pub fn timestamp_to_beijing_string(timestamp: i64) -> String {
    match DateTime::from_timestamp(timestamp, 0) {
        Some(dt) => dt
            .with_timezone(&beijing_timezone())
            .format(STANDARD_TIME_FORMAT)
            .to_string(),
        None => now_standard_string(), // 如果时间戳无效，返回当前时间
    }
}

/// 将 Unix 时间戳转换为标准格式字符串（北京时间） - 兼容性别名
pub fn timestamp_to_standard_string(timestamp: i64) -> String {
    timestamp_to_beijing_string(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_format() {
        let time_str = now_standard_string();
        assert!(time_str.len() == 19); // YYYY-MM-DD HH:MM:SS
        assert!(!time_str.contains('T'));
        assert!(!time_str.contains('+'));
        assert!(!time_str.contains('.'));
    }

    #[test]
    fn test_parse_various_formats() {
        // 测试各种格式的解析
        let formats = vec![
            "2025-07-25 16:52:53",
            "2025-07-25 16:52:53.827266",
            "2025-07-25 16:52:53.827266 +08:00",
            "2025-07-25 16:52:53 +08:00",
            "2025-07-25T16:52:53.827266200+08:00",
        ];

        for format in formats {
            let parsed = parse_time_string(format);
            assert!(parsed.is_some(), "Failed to parse: {}", format);
        }
    }
}
