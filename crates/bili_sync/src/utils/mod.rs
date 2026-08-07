pub mod ai_rename;
pub mod bangumi_cache;
pub mod bangumi_name_extractor;
pub mod collection_aggregate;
pub mod convert;
pub mod danmaku_schedule;
pub mod deepseek_pow;
pub mod deepseek_web;
pub mod file_logger;
pub mod filenamify;
pub mod format_arg;
pub mod keyword_filter;
pub mod live_updates;
pub mod model;
pub mod netscape_cookies;
pub mod nfo;
pub mod notification;
pub mod scan_collector;
pub mod scan_id_tracker;
pub mod signal;
pub mod status;
pub mod submission_checkpoint;
pub mod task_notifier;
pub mod time_format;

use std::fmt;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

// 自定义日志层，用于将日志添加到API缓冲区
struct LogCaptureLayer;

impl<S> Layer<S> for LogCaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        use crate::api::handler::{add_log_entry, LogLevel};
        use crate::utils::time_format::now_standard_string;

        let level = match *event.metadata().level() {
            tracing::Level::ERROR => LogLevel::Error,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::TRACE => LogLevel::Debug, // 将TRACE映射到DEBUG
        };

        let level_str = match *event.metadata().level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "debug",
        };

        // 提取日志消息
        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);

        if let Some(mut message) = visitor.message {
            if let Some(error) = visitor.error.filter(|error| !error.trim().is_empty()) {
                message.push_str("：");
                message.push_str(&error);
            }
            let mut target = event.metadata().target().to_string();
            if target == "bili_sync_rs::youtube"
                && (visitor
                    .platform
                    .as_deref()
                    .is_some_and(|platform| platform.eq_ignore_ascii_case("douyin") || platform == "抖音")
                    || message.starts_with("抖音")
                    || message.starts_with("下载抖音")
                    || message.starts_with("扫描抖音"))
            {
                target = "bili_sync_rs::douyin".to_string();
            }

            // 写入文件日志
            if let Some(ref writer) = *file_logger::FILE_LOG_WRITER {
                writer.write_log(&now_standard_string(), level_str, &message, Some(&target));
            }

            // 添加到内存缓冲区
            add_log_entry(level, message, Some(target));
        }
    }
}

// 用于提取日志消息的访问者
struct MessageVisitor {
    message: Option<String>,
    error: Option<String>,
    platform: Option<String>,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: None,
            error: None,
            platform: None,
        }
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        let value = format!("{:?}", value);
        match field.name() {
            "message" => self.message = Some(value),
            "error" => self.error = Some(value.trim_matches('"').to_string()),
            "platform" => self.platform = Some(value.trim_matches('"').to_string()),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_string()),
            "error" => self.error = Some(value.to_string()),
            "platform" => self.platform = Some(value.to_string()),
            _ => {}
        }
    }
}

pub fn init_logger(log_level: &str) {
    // 构建优化的日志过滤器，降低sqlx慢查询等噪音
    let console_filter = build_optimized_filter(log_level);
    let api_filter = build_optimized_filter("debug");

    // 控制台输出层 - 使用优化的过滤器
    let fmt_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new(
            "%b %d %H:%M:%S".to_owned(),
        ))
        .with_filter(console_filter);

    // API日志捕获层 - 使用优化的过滤器
    let log_capture_layer = LogCaptureLayer.with_filter(api_filter);

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(log_capture_layer)
        .try_init()
        .expect("初始化日志失败");
}

/// 构建优化的日志过滤器，减少噪音日志
fn build_optimized_filter(base_level: &str) -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::builder().parse_lossy(format!(
        "{},\
            sqlx::query=error,\
            sqlx=error,\
            sea_orm::database=error,\
            sea_orm_migration=warn,\
            tokio_util=warn,\
            hyper=warn,\
            reqwest=warn,\
            h2=warn",
        base_level
    ))
}
