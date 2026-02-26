use std::io;

use anyhow::Result;
use thiserror::Error;

#[derive(Error, Debug)]
#[error("Request too frequently")]
pub struct DownloadAbortError();

#[derive(Error, Debug)]
pub struct ProcessPageError {
    pub video_name: String,
    pub status_code: u32,
    pub details: Option<String>,
}

impl ProcessPageError {
    pub fn new(video_name: String, status_code: u32) -> Self {
        Self {
            video_name,
            status_code,
            details: None,
        }
    }

    pub fn with_details(mut self, details: String) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for ProcessPageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ref details) = self.details {
            write!(
                f,
                "视频「{}」分页下载失败，状态码: {}。{}",
                self.video_name, self.status_code, details
            )
        } else {
            write!(
                f,
                "视频「{}」分页下载失败，状态码: {}。请检查网络连接、文件系统权限或重试下载。",
                self.video_name, self.status_code
            )
        }
    }
}

/// 错误类型枚举，用于更精确的错误分类
#[derive(Error, Debug, Clone, PartialEq)]
pub enum ErrorType {
    #[error("网络连接错误")]
    Network,
    #[error("权限不足")]
    Permission,
    #[error("认证失败")]
    Authentication,
    #[error("授权失败")]
    Authorization,
    #[error("资源未找到")]
    NotFound,
    #[error("请求过于频繁")]
    RateLimit,
    #[error("服务器内部错误")]
    ServerError,
    #[error("客户端错误")]
    ClientError,
    #[error("解析错误")]
    Parse,
    #[error("超时错误")]
    Timeout,
    #[error("文件系统错误")]
    FileSystem,
    #[error("配置错误")]
    Configuration,
    #[error("风控触发")]
    RiskControl,
    #[error("用户主动暂停")]
    UserCancelled,
    #[error("未知错误")]
    Unknown,
}

/// 分类后的错误信息
#[derive(Error, Debug)]
#[error("{error_type}: {message}")]
pub struct ClassifiedError {
    pub error_type: ErrorType,
    pub message: String,
    pub status_code: Option<u16>,
    pub should_retry: bool,
    pub should_ignore: bool,
    /// 是否需要自动删除（现在主要由upower字段检测处理）
    pub should_auto_delete: bool,
}

impl ClassifiedError {
    pub fn new(error_type: ErrorType, message: String) -> Self {
        let (should_retry, should_ignore) = match error_type {
            ErrorType::Network | ErrorType::Timeout | ErrorType::RateLimit => (true, false),
            ErrorType::Permission | ErrorType::FileSystem => (false, true),
            ErrorType::NotFound => (false, false),
            ErrorType::Authentication | ErrorType::Authorization => (false, false),
            ErrorType::RiskControl => (false, false),
            ErrorType::UserCancelled => (false, true), // 用户主动暂停：不重试，直接忽略
            ErrorType::ServerError => (true, false),
            ErrorType::ClientError | ErrorType::Parse | ErrorType::Configuration => (false, false),
            ErrorType::Unknown => (false, false),
        };

        Self {
            error_type,
            message,
            status_code: None,
            should_retry,
            should_ignore,
            should_auto_delete: false, // 默认不自动删除
        }
    }

    pub fn with_status_code(mut self, status_code: u16) -> Self {
        self.status_code = Some(status_code);
        self
    }

    pub fn with_retry_policy(mut self, should_retry: bool, should_ignore: bool) -> Self {
        self.should_retry = should_retry;
        self.should_ignore = should_ignore;
        self
    }

    pub fn with_auto_delete(mut self, should_auto_delete: bool) -> Self {
        self.should_auto_delete = should_auto_delete;
        self
    }
}

/// 错误分类器
pub struct ErrorClassifier;

impl ErrorClassifier {
    fn is_not_found_like_message(message: &str) -> bool {
        let message_lower = message.to_lowercase();
        message_lower.contains("status code: -404")
            || message_lower.contains("啥都木有")
            || message_lower.contains("not found")
            || message_lower.contains("无内容")
            || message_lower.contains("视频不存在")
            || message_lower.contains("视频已被删除")
    }

    fn is_not_found_page_error(status_code: u32, details: Option<&str>) -> bool {
        status_code == 404 || details.is_some_and(Self::is_not_found_like_message)
    }

    /// 分析并分类错误
    pub fn classify_error(err: &anyhow::Error) -> ClassifiedError {
        let error_msg = err.to_string();

        // 首先检查是否为用户主动暂停相关错误
        if Self::is_user_cancellation(&error_msg) {
            return ClassifiedError::new(ErrorType::UserCancelled, "用户主动暂停任务".to_string());
        }

        // 检查是否为充电专享视频相关错误（扩展检测，优先级最高）
        // 注意：只检测明确的充电视频错误消息，不再将"所有质量级别都获取失败"误判为充电视频
        // 充电视频检测应该在获取视频详情时通过 is_upower_exclusive 字段进行
        if error_msg.contains("充电专享视频")
            || error_msg.contains("需要为UP主充电才能观看")
            || error_msg.contains("视频需要充电才能观看")
        {
            return ClassifiedError::new(ErrorType::Permission, "充电专享视频，需要为UP主充电才能观看".to_string())
                .with_retry_policy(false, true) // 不重试，可忽略
                .with_auto_delete(true); // 需要自动删除
        }

        for cause in err.chain() {
            // DownloadAbortError 用于风控/请求频率过高，不是充电视频
            if cause.downcast_ref::<DownloadAbortError>().is_some() {
                return ClassifiedError::new(ErrorType::RiskControl, "请求过于频繁，可能触发风控".to_string())
                    .with_retry_policy(false, false); // 不重试，不可忽略（需要处理风控）
            }

            // HTTP 状态码错误
            if let Some(reqwest_err) = cause.downcast_ref::<reqwest::Error>() {
                return Self::classify_reqwest_error(reqwest_err);
            }

            // IO 错误
            if let Some(io_err) = cause.downcast_ref::<io::Error>() {
                return Self::classify_io_error(io_err);
            }

            // B站特定错误
            if let Some(bili_err) = cause.downcast_ref::<crate::bilibili::BiliError>() {
                return Self::classify_bili_error(bili_err);
            }

            // 分页错误处理
            if let Some(page_error) = cause.downcast_ref::<ProcessPageError>() {
                // 如果在暂停状态下发生分页错误，很可能是因为用户暂停导致的
                if crate::task::TASK_CONTROLLER.is_paused() {
                    return ClassifiedError::new(ErrorType::UserCancelled, "分页下载因用户暂停而终止".to_string());
                }

                if Self::is_not_found_page_error(page_error.status_code, page_error.details.as_deref()) {
                    let message = page_error
                        .details
                        .as_deref()
                        .map(|d| format!("分页资源不存在或已下架: {}", d))
                        .unwrap_or_else(|| "分页资源不存在或已下架".to_string());
                    return ClassifiedError::new(ErrorType::NotFound, message).with_retry_policy(false, false);
                }

                // 使用ProcessPageError自带的详细信息
                return ClassifiedError::new(ErrorType::Unknown, page_error.to_string());
            }

            // JSON解析错误
            if cause.downcast_ref::<serde_json::Error>().is_some() {
                return ClassifiedError::new(ErrorType::Parse, "JSON解析失败".to_string());
            }
        }

        ClassifiedError::new(ErrorType::Unknown, err.to_string())
    }

    fn classify_reqwest_error(err: &reqwest::Error) -> ClassifiedError {
        if err.is_timeout() {
            return ClassifiedError::new(ErrorType::Timeout, "请求超时".to_string());
        }

        if err.is_connect() {
            return ClassifiedError::new(ErrorType::Network, "网络连接失败".to_string());
        }

        if err.is_decode() || err.is_body() {
            return ClassifiedError::new(ErrorType::Parse, "响应解析失败".to_string());
        }

        if let Some(status) = err.status() {
            let status_code = status.as_u16();
            let _error_message = err.to_string();

            // HTTP 412 错误直接归类为风控，不再检测充电视频
            // 充电视频检测在获取视频信息时已经完成

            let error_type = match status_code {
                401 => ErrorType::Authentication,
                403 => ErrorType::Authorization,
                404 => ErrorType::NotFound,
                412 => ErrorType::RiskControl, // HTTP 412 始终是风控
                429 => ErrorType::RateLimit,
                500..=599 => ErrorType::ServerError,
                400..=499 => ErrorType::ClientError,
                _ => ErrorType::Unknown,
            };

            let message = match status_code {
                401 => "认证失败，请检查登录状态".to_string(),
                403 => "权限不足，无法访问该资源".to_string(),
                404 => "请求的资源不存在".to_string(),
                412 => "触发B站风控限制，需要进行验证".to_string(),
                429 => "请求过于频繁，请稍后重试".to_string(),
                500..=599 => "服务器内部错误".to_string(),
                _ => format!("HTTP错误: {}", status_code),
            };

            let classified_error = ClassifiedError::new(error_type.clone(), message).with_status_code(status_code);

            // HTTP 412 是风控错误，不需要自动删除，应触发风控验证流程

            return classified_error;
        }

        ClassifiedError::new(ErrorType::Network, "网络请求失败".to_string())
    }

    fn classify_io_error(err: &io::Error) -> ClassifiedError {
        match err.kind() {
            io::ErrorKind::PermissionDenied => ClassifiedError::new(ErrorType::Permission, "文件权限不足".to_string()),
            io::ErrorKind::NotFound => ClassifiedError::new(ErrorType::FileSystem, "文件或目录不存在".to_string()),
            io::ErrorKind::ConnectionRefused => ClassifiedError::new(ErrorType::Network, "连接被拒绝".to_string()),
            io::ErrorKind::TimedOut => ClassifiedError::new(ErrorType::Timeout, "操作超时".to_string()),
            io::ErrorKind::WriteZero | io::ErrorKind::UnexpectedEof => {
                ClassifiedError::new(ErrorType::Network, "网络连接中断".to_string())
            }
            _ => ClassifiedError::new(ErrorType::FileSystem, format!("文件系统错误: {}", err)),
        }
    }

    fn classify_bili_error(err: &crate::bilibili::BiliError) -> ClassifiedError {
        match err {
            crate::bilibili::BiliError::RiskControlOccurred => {
                ClassifiedError::new(ErrorType::RiskControl, "触发B站风控，请稍后重试".to_string())
                    .with_retry_policy(false, false) // 风控不重试，不忽略
            }
            crate::bilibili::BiliError::RiskControlVerificationRequired(v_voucher) => {
                ClassifiedError::new(
                    ErrorType::RiskControl,
                    format!("需要风控验证 (v_voucher: {})", v_voucher),
                )
                .with_retry_policy(false, true) // 不重试，可忽略（跳过该视频）
            }
            crate::bilibili::BiliError::VideoStreamEmpty(msg) => {
                ClassifiedError::new(ErrorType::NotFound, format!("视频流为空: {}", msg))
                    .with_retry_policy(false, false) // 不重试，不忽略
                    .with_auto_delete(false) // 不自动删除，这可能是地区限制等其他原因
            }
            crate::bilibili::BiliError::RequestFailed(code, msg) => {
                let error_type = match *code {
                    -352 | -412 => ErrorType::RiskControl, // 特定风控错误码
                    -401 | -403 => ErrorType::Authentication,
                    -404 => ErrorType::NotFound,
                    -429 => ErrorType::RateLimit,
                    -500..=-400 => ErrorType::ServerError,
                    // 充电专享视频错误现在通过upower字段在获取详情时处理，不需要特殊分类
                    _ => ErrorType::ClientError,
                };

                let should_retry = match *code {
                    -352 | -412 => false,     // 风控不重试
                    -404 => false,            // 404错误不重试
                    -500..=-400 | -1 => true, // 服务器错误或网络错误可重试
                    _ => false,               // 其他错误不重试
                };

                let should_ignore = match *code {
                    -404 => false, // 404错误不忽略，按失败处理
                    _ => false,    // 其他错误不忽略
                };

                let message = format!("B站API错误: {}", msg);

                ClassifiedError::new(error_type, message)
                    .with_retry_policy(should_retry, should_ignore)
                    .with_auto_delete(false) // 不自动删除
            }
        }
    }

    /// 检查错误信息是否表示用户主动取消
    fn is_user_cancellation(error_msg: &str) -> bool {
        // 检查是否处于暂停状态
        let is_paused = crate::task::TASK_CONTROLLER.is_paused();

        // 明确的暂停相关错误信息（优先检查）
        let explicit_cancellation = error_msg.contains("任务已暂停")
            || error_msg.contains("停止下载")
            || error_msg.contains("用户主动暂停任务")
            || error_msg.contains("用户暂停任务")
            || error_msg.contains("因用户暂停而终止");

        if explicit_cancellation {
            return true;
        }

        // 暂停状态下的特定错误信息
        if is_paused {
            return error_msg.contains("Download cancelled")
                || error_msg.contains("由于目标计算机积极拒绝，无法连接")
                || error_msg.contains("os error 10061")
                || error_msg.contains("Connection refused");
        }

        false
    }
}

pub enum ExecutionStatus {
    Skipped,
    Succeeded,
    /// 用户暂停/取消导致的提前结束：不应当把对应子任务标记为已完成
    ///（否则会出现“暂停后仍显示已下载完成”的误判）。
    Cancelled,
    Ignored(anyhow::Error),
    Failed(anyhow::Error),
    // 任务可以返回该状态固定自己的 status
    FixedFailed(u32, anyhow::Error),
    // 新增：分类后的错误状态
    ClassifiedFailed(ClassifiedError),
}

// 目前 stable rust 似乎不支持自定义类型使用 ? 运算符，只能先在返回值使用 Result，再这样套层娃
impl From<Result<ExecutionStatus>> for ExecutionStatus {
    fn from(res: Result<ExecutionStatus>) -> Self {
        match res {
            Ok(status) => status,
            Err(err) => {
                let classified_error = ErrorClassifier::classify_error(&err);

                // 根据分类结果决定处理方式
                // 如果需要自动删除，使用ClassifiedFailed以便被检测到
                if classified_error.should_auto_delete {
                    return ExecutionStatus::ClassifiedFailed(classified_error);
                }

                if classified_error.should_ignore {
                    return ExecutionStatus::Ignored(err);
                }

                // 检查传统的忽略条件（向后兼容）
                for cause in err.chain() {
                    if let Some(io_err) = cause.downcast_ref::<io::Error>() {
                        // 权限错误
                        if io_err.kind() == io::ErrorKind::PermissionDenied {
                            return ExecutionStatus::Ignored(err);
                        }
                        // 使用 io::Error 包裹的 reqwest::Error
                        if io_err.kind() == io::ErrorKind::Other
                            && io_err.get_ref().is_some_and(|e| {
                                e.downcast_ref::<reqwest::Error>().is_some_and(is_ignored_reqwest_error)
                            })
                        {
                            return ExecutionStatus::Ignored(err);
                        }
                    }
                    // 未包裹的 reqwest::Error
                    if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
                        if is_ignored_reqwest_error(error) {
                            return ExecutionStatus::Ignored(err);
                        }
                    }
                }
                ExecutionStatus::ClassifiedFailed(classified_error)
            }
        }
    }
}

fn is_ignored_reqwest_error(err: &reqwest::Error) -> bool {
    err.is_decode() || err.is_body() || err.is_timeout()
}
