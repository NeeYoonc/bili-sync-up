use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use serde::{Deserialize, Serialize};

mod bundle;
mod clap;
mod global;
mod item;
mod manager;

use crate::bilibili::{Credential, DanmakuOption, FilterOption};
pub(crate) use crate::config::bundle::validate_naming_template_syntax;
pub use crate::config::bundle::ConfigBundle;
pub use crate::config::clap::version;
pub use crate::config::global::{
    get_config_manager, init_config_with_database, reload_config, reload_config_bundle, with_config, ARGS,
    CONFIG_BUNDLE, CONFIG_DIR,
};
use crate::config::item::ConcurrentLimit;
pub use crate::config::item::{
    DanmakuUpdatePolicy, EmptyUpperStrategy, NFOConfig, NFOTimeType, PathSafeTemplate, RateLimit,
    SubmissionRiskControlConfig, SubmissionScanStrategyConfig,
};
pub(crate) use crate::config::manager::describe_config_key;
pub use crate::config::manager::ConfigManager;

// 移除不再需要的配置结构体，因为视频源现在存储在数据库中
// #[derive(Serialize, Deserialize, Default, Debug, Clone)]
// pub struct BangumiConfig {
//     pub season_id: Option<String>,
//     pub media_id: Option<String>,
//     pub ep_id: Option<String>,
//     pub path: PathBuf,
//     #[serde(default = "default_download_all_seasons")]
//     pub download_all_seasons: bool,
//     /// 番剧专用的 video_name 模板，如果未设置则使用全局配置
//     #[serde(default)]
//     pub video_name: Option<String>,
//     /// 番剧专用的 page_name 模板，如果未设置则使用全局 bangumi_name 配置
//     #[serde(default)]
//     pub page_name: Option<String>,
// }

// #[derive(Serialize, Deserialize, Default, Debug, Clone)]
// pub struct FavoriteConfig {
//     pub fid: String,
//     pub path: PathBuf,
//     #[serde(default = "default_download_all_seasons")]
//     pub download_all_seasons: bool,
//     #[serde(default = "default_page_name")]
//     pub page_name: Option<String>,
// }

// #[derive(Serialize, Deserialize, Default, Debug, Clone)]
// pub struct CollectionConfig {
//     pub collection_type: String, // "season" 或 "series"
//     pub upper_id: String,
//     pub collection_id: String,
//     pub path: PathBuf,
//     #[serde(default = "default_download_all_seasons")]
//     pub download_all_seasons: bool,
//     #[serde(default = "default_page_name")]
//     pub page_name: Option<String>,
// }

// #[derive(Serialize, Deserialize, Default, Debug, Clone)]
// pub struct SubmissionConfig {
//     pub upper_id: String,
//     pub path: PathBuf,
//     #[serde(default = "default_download_all_seasons")]
//     pub download_all_seasons: bool,
//     #[serde(default = "default_page_name")]
//     pub page_name: Option<String>,
// }

// #[derive(Serialize, Deserialize, Default, Debug, Clone)]
// pub struct WatchLaterConfig {
//     #[serde(default)]
//     pub enabled: bool,
//     #[serde(default)]
//     pub path: PathBuf,
// }

fn default_time_format() -> String {
    "%Y%m%d%H%M%S".to_string()
}

/// 默认的 auth_token 实现，首次使用时返回None，需要用户主动设置
fn default_auth_token() -> Option<String> {
    // 首次使用时不自动生成token，需要用户通过初始设置界面设置
    None
}

fn default_bind_address() -> String {
    "0.0.0.0:12345".to_string()
}

fn default_video_name() -> Cow<'static, str> {
    Cow::Borrowed("{{upper_name}}/{{title}}")
}

fn default_page_name() -> Cow<'static, str> {
    Cow::Borrowed("{{pubtime}}-{{bvid}}")
}

fn default_interval() -> u64 {
    1200
}

fn default_upper_path() -> PathBuf {
    CONFIG_DIR.join("upper_face")
}

fn default_quick_subscribe_path_template() -> Cow<'static, str> {
    Cow::Borrowed("")
}

fn default_ffmpeg_path() -> String {
    String::new()
}

fn default_split_chapters_after_download() -> bool {
    false
}

// 移除不再需要的默认函数
// fn default_download_all_seasons() -> bool {
//     false
// }

// fn default_page_name() -> Option<String> {
//     Some("{{title}}".to_string())
// }

fn default_multi_page_name() -> Cow<'static, str> {
    Cow::Borrowed("P{{pid_pad}}.{{ptitle}}")
}

fn default_bangumi_name() -> Cow<'static, str> {
    Cow::Borrowed("S{{season_pad}}E{{pid_pad}}")
}

fn default_collection_unified_name() -> Cow<'static, str> {
    Cow::Borrowed("S{{season_pad}}E{{episode_pad}}{{#if is_multi_page}}P{{pid_pad}}{{/if}} - {{title}}")
}

fn default_folder_structure() -> Cow<'static, str> {
    Cow::Borrowed("Season {{season_pad}}")
}

fn default_bangumi_folder_name() -> Cow<'static, str> {
    Cow::Borrowed("{{series_title}}")
}

fn default_collection_folder_mode() -> Cow<'static, str> {
    Cow::Borrowed("unified") // 默认为统一模式
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_auth_token")]
    pub auth_token: Option<String>,
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default)]
    pub credential: ArcSwapOption<Credential>,
    #[serde(default)]
    pub filter_option: FilterOption,
    #[serde(default)]
    pub danmaku_option: DanmakuOption,
    #[serde(default)]
    pub danmaku_update_policy: DanmakuUpdatePolicy,
    #[serde(default = "default_video_name")]
    pub video_name: Cow<'static, str>,
    #[serde(default = "default_page_name")]
    pub page_name: Cow<'static, str>,
    #[serde(default = "default_multi_page_name")]
    pub multi_page_name: Cow<'static, str>,
    #[serde(default = "default_bangumi_name")]
    pub bangumi_name: Cow<'static, str>,
    #[serde(default = "default_folder_structure")]
    pub folder_structure: Cow<'static, str>,
    #[serde(default = "default_bangumi_folder_name")]
    pub bangumi_folder_name: Cow<'static, str>,
    #[serde(default = "default_collection_folder_mode")]
    pub collection_folder_mode: Cow<'static, str>,
    #[serde(default = "default_collection_unified_name")]
    pub collection_unified_name: Cow<'static, str>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(default = "default_upper_path")]
    pub upper_path: PathBuf,
    #[serde(default = "default_quick_subscribe_path_template")]
    pub favorite_quick_subscribe_path: Cow<'static, str>,
    #[serde(default = "default_quick_subscribe_path_template")]
    pub collection_quick_subscribe_path: Cow<'static, str>,
    #[serde(default = "default_quick_subscribe_path_template")]
    pub submission_quick_subscribe_path: Cow<'static, str>,
    #[serde(default = "default_quick_subscribe_path_template")]
    pub bangumi_quick_subscribe_path: Cow<'static, str>,
    #[serde(default = "default_quick_subscribe_path_template")]
    pub pugv_quick_subscribe_path: Cow<'static, str>,
    /// 漫画（哔哩哔哩漫画）快捷订阅路径模板
    #[serde(default = "default_quick_subscribe_path_template")]
    pub manga_quick_subscribe_path: Cow<'static, str>,
    /// 可选的 ffmpeg 路径（可填 ffmpeg.exe 文件路径或其所在目录）
    #[serde(default = "default_ffmpeg_path")]
    pub ffmpeg_path: String,
    #[serde(default = "default_split_chapters_after_download")]
    pub split_chapters_after_download: bool,
    /// 外源网络代理（YouTube/TikTok 等平台的 yt-dlp 与直链下载共用）。
    #[serde(default)]
    pub proxy: String,
    /// 兼容旧配置：早期仅供 YouTube 使用的代理字段，新值写入 `proxy`。
    #[serde(default)]
    pub youtube_proxy: String,
    #[serde(default)]
    pub nfo_time_type: NFOTimeType,
    #[serde(default)]
    pub nfo_config: NFOConfig,
    #[serde(default)]
    pub concurrent_limit: ConcurrentLimit,
    #[serde(default = "default_time_format")]
    pub time_format: String,
    #[serde(default = "default_cdn_sorting")]
    pub cdn_sorting: bool,
    #[serde(default)]
    pub submission_risk_control: crate::config::item::SubmissionRiskControlConfig,
    /// B站合集/收藏夹/UP主投稿源扫描策略（分批/自适应）
    #[serde(default)]
    pub submission_scan_strategy: SubmissionScanStrategyConfig,
    #[serde(default)]
    pub scan_deleted_videos: bool,
    // 番剧预告片过滤配置
    #[serde(default = "default_skip_bangumi_preview")]
    pub skip_bangumi_preview: bool,
    // aria2监控相关配置
    #[serde(default)]
    pub enable_aria2_health_check: bool,
    #[serde(default)]
    pub enable_aria2_auto_restart: bool,
    #[serde(default = "default_aria2_health_check_interval")]
    pub aria2_health_check_interval: u64,
    // actors字段初始化状态标记
    #[serde(default)]
    pub actors_field_initialized: bool,
    // 多P视频是否使用Season文件夹结构
    #[serde(default = "default_multi_page_use_season_structure")]
    pub multi_page_use_season_structure: bool,
    // 合集是否使用Season文件夹结构
    #[serde(default = "default_collection_use_season_structure")]
    pub collection_use_season_structure: bool,
    // 番剧是否使用Season文件夹结构（同时启用系列名标准化）
    #[serde(default = "default_bangumi_use_season_structure")]
    pub bangumi_use_season_structure: bool,
    // 结构变化（如单P变多P）后是否自动把已下载的分页文件迁入 Season 目录
    #[serde(default = "default_auto_reorganize_pages")]
    pub auto_reorganize_pages: bool,
    /// 文件名严格清洗：去掉标题里的 emoji、颜文字等特殊字符
    #[serde(default = "default_filename_strict_cleaning")]
    pub filename_strict_cleaning: bool,
    /// 下载暂存目录：设置后下载中的中间文件先写到该目录，合并与校验通过后再移动到最终位置。
    /// 留空表示与旧版本行为一致（中间文件直接写在目标文件旁边）。
    #[serde(default)]
    pub download_staging_dir: Option<PathBuf>,
    /// 历史下载暂存目录：切换下载暂存目录时保留旧根，用于启动统计与手动清理残留。
    /// 只由程序维护，不在设置页展示，也不接受配置接口直接写入。
    #[serde(default)]
    pub download_staging_previous_dirs: Vec<PathBuf>,
    /// 自动创建过的下载暂存目录（下载暂存目录留空时按视频源目录自动派生）：
    /// 用于启动统计与「清理暂存残留」覆盖这些隐藏目录。只由程序维护。
    #[serde(default)]
    pub download_staging_auto_dirs: Vec<PathBuf>,
    // 推送通知配置
    #[serde(default)]
    pub notification: NotificationConfig,
    // 启动时数据修复功能开关（默认关闭以减少不必要的日志）
    #[serde(default)]
    pub enable_startup_data_fix: bool,
    // 启动时填充缺失视频CID功能开关（默认关闭）
    #[serde(default)]
    pub enable_cid_population: bool,
    // 风控验证配置
    #[serde(default)]
    pub risk_control: RiskControlConfig,

    /// AI 自动重命名配置（OpenAI 兼容接口）
    #[serde(default)]
    pub ai_rename: crate::utils::ai_rename::AiRenameConfig,
}

fn default_skip_bangumi_preview() -> bool {
    true // 默认跳过预告片
}

fn default_aria2_health_check_interval() -> u64 {
    300 // 默认5分钟
}

fn default_multi_page_use_season_structure() -> bool {
    true // 默认使用Season结构
}

fn default_collection_use_season_structure() -> bool {
    true // 默认使用Season结构
}

fn default_bangumi_use_season_structure() -> bool {
    true // 默认使用Season结构（同时启用系列名标准化）
}

fn default_auto_reorganize_pages() -> bool {
    true // 默认在结构变化后自动整理已下载的分页文件
}

fn default_filename_strict_cleaning() -> bool {
    true // 默认严格清洗文件名（去掉 emoji / 颜文字）
}

fn default_cdn_sorting() -> bool {
    true // 默认启用CDN排序
}

// 推送通知配置结构体
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NotificationConfig {
    // === 当前激活的通知渠道 ===
    #[serde(default = "default_active_channel")]
    pub active_channel: String, // "none", "serverchan", "serverchan3", "wecom", "webhook"

    // === Server酱配置 ===
    #[serde(default)]
    pub serverchan_key: Option<String>,

    // === Server酱3配置 ===
    #[serde(default)]
    pub serverchan3_uid: Option<String>,
    #[serde(default)]
    pub serverchan3_sendkey: Option<String>,

    // === 企业微信群机器人配置 ===
    #[serde(default)]
    pub wecom_webhook_url: Option<String>,
    #[serde(default = "default_wecom_msgtype")]
    pub wecom_msgtype: String,
    #[serde(default)]
    pub wecom_mention_all: bool,
    #[serde(default)]
    pub wecom_mentioned_list: Option<Vec<String>>,

    // === 通用Webhook配置 ===
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub webhook_bearer_token: Option<String>,
    #[serde(default)]
    pub webhook_custom_headers: Option<String>,
    #[serde(default = "default_webhook_format")]
    pub webhook_format: String, // "auto", "generic", "opensend", "custom", "synology_chat"
    #[serde(default)]
    pub webhook_custom_body: Option<String>,
    /// 仅用于 Synology Chat：消息文本模板，不是 JSON。
    #[serde(default)]
    pub webhook_synology_chat_template: Option<String>,

    // === 通用配置 ===
    #[serde(default)]
    pub enable_scan_notifications: bool,
    #[serde(default = "default_notification_min_videos")]
    pub notification_min_videos: usize,
    #[serde(default = "default_notification_timeout")]
    pub notification_timeout: u64,
    #[serde(default = "default_notification_retry_count")]
    pub notification_retry_count: u8,
}

fn default_notification_min_videos() -> usize {
    1
}

fn default_notification_timeout() -> u64 {
    10
}

fn default_notification_retry_count() -> u8 {
    3
}

fn default_wecom_msgtype() -> String {
    "markdown".to_string()
}

fn default_active_channel() -> String {
    "none".to_string()
}

fn default_webhook_format() -> String {
    "auto".to_string()
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            active_channel: default_active_channel(),
            serverchan_key: None,
            serverchan3_uid: None,
            serverchan3_sendkey: None,
            wecom_webhook_url: None,
            wecom_msgtype: default_wecom_msgtype(),
            wecom_mention_all: false,
            wecom_mentioned_list: None,
            webhook_url: None,
            webhook_bearer_token: None,
            webhook_custom_headers: None,
            webhook_format: default_webhook_format(),
            webhook_custom_body: None,
            webhook_synology_chat_template: None,
            enable_scan_notifications: false,
            notification_min_videos: default_notification_min_videos(),
            notification_timeout: default_notification_timeout(),
            notification_retry_count: default_notification_retry_count(),
        }
    }
}

impl NotificationConfig {
    /// 为旧配置自动推断 active_channel
    pub fn infer_active_channel(&mut self) {
        if self.webhook_format.trim().is_empty() {
            self.webhook_format = default_webhook_format();
        }

        if self.active_channel != "none" {
            return;
        }

        // 优先选择 Server酱
        if self.serverchan_key.is_some() && !self.serverchan_key.as_ref().unwrap().is_empty() {
            self.active_channel = "serverchan".to_string();
        }
        // 其次选择 Server酱3
        else if self.serverchan3_uid.is_some()
            && self.serverchan3_sendkey.is_some()
            && !self.serverchan3_uid.as_ref().unwrap().is_empty()
            && !self.serverchan3_sendkey.as_ref().unwrap().is_empty()
        {
            self.active_channel = "serverchan3".to_string();
        }
        // 最后选择企业微信
        else if self.wecom_webhook_url.is_some() && !self.wecom_webhook_url.as_ref().unwrap().is_empty() {
            self.active_channel = "wecom".to_string();
        }
        // 再次选择通用Webhook
        else if self.webhook_url.is_some() && !self.webhook_url.as_ref().unwrap().is_empty() {
            self.active_channel = "webhook".to_string();
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        // 验证 active_channel 的有效性
        if !["none", "serverchan", "serverchan3", "wecom", "webhook"].contains(&self.active_channel.as_str()) {
            return Err(format!("无效的通知渠道: {}", self.active_channel));
        }

        // 启用推送时，如果未显式选择渠道，则尝试按旧配置规则推断渠道
        let active_channel = if self.enable_scan_notifications && self.active_channel == "none" {
            // 优先选择 Server酱
            if self.serverchan_key.is_some() && !self.serverchan_key.as_ref().unwrap().is_empty() {
                "serverchan"
            }
            // 其次选择 Server酱3
            else if self.serverchan3_uid.is_some()
                && self.serverchan3_sendkey.is_some()
                && !self.serverchan3_uid.as_ref().unwrap().is_empty()
                && !self.serverchan3_sendkey.as_ref().unwrap().is_empty()
            {
                "serverchan3"
            }
            // 最后选择企业微信
            else if self.wecom_webhook_url.is_some() && !self.wecom_webhook_url.as_ref().unwrap().is_empty() {
                "wecom"
            }
            // 再次选择通用Webhook
            else if self.webhook_url.is_some() && !self.webhook_url.as_ref().unwrap().is_empty() {
                "webhook"
            } else {
                "none"
            }
        } else {
            self.active_channel.as_str()
        };

        // 如果启用推送，必须选择一个渠道
        if self.enable_scan_notifications {
            if active_channel == "none" {
                return Err("启用推送通知时必须选择一个通知渠道".to_string());
            }

            // 验证选中渠道的配置
            match active_channel {
                "serverchan" => {
                    if self.serverchan_key.is_none() || self.serverchan_key.as_ref().unwrap().is_empty() {
                        return Err("已选择Server酱但未配置密钥".to_string());
                    }
                }
                "serverchan3" => {
                    if self.serverchan3_uid.is_none() || self.serverchan3_uid.as_ref().unwrap().is_empty() {
                        return Err("已选择Server酱3但未配置UID".to_string());
                    }
                    if self.serverchan3_sendkey.is_none() || self.serverchan3_sendkey.as_ref().unwrap().is_empty() {
                        return Err("已选择Server酱3但未配置SendKey".to_string());
                    }
                }
                "wecom" => {
                    if self.wecom_webhook_url.is_none() || self.wecom_webhook_url.as_ref().unwrap().is_empty() {
                        return Err("已选择企业微信但未配置Webhook URL".to_string());
                    }

                    let url = self.wecom_webhook_url.as_ref().unwrap();
                    if !url.starts_with("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=") {
                        return Err("企业微信Webhook URL格式不正确".to_string());
                    }

                    if !matches!(self.wecom_msgtype.as_str(), "text" | "markdown") {
                        return Err("企业微信消息类型必须是 'text' 或 'markdown'".to_string());
                    }
                }
                "webhook" => {
                    if self.webhook_url.is_none() || self.webhook_url.as_ref().unwrap().is_empty() {
                        return Err("已选择Webhook但未配置Webhook URL".to_string());
                    }
                    let url = self.webhook_url.as_ref().unwrap();
                    if !(url.starts_with("http://") || url.starts_with("https://")) {
                        return Err("Webhook URL格式不正确".to_string());
                    }
                    if !["auto", "generic", "opensend", "custom", "synology_chat"]
                        .contains(&self.webhook_format.as_str())
                    {
                        return Err(format!("Webhook格式不支持: {}", self.webhook_format));
                    }
                    if self.webhook_format == "custom"
                        && self.webhook_custom_body.as_ref().is_none_or(|v| v.trim().is_empty())
                    {
                        return Err("已选择自定义请求 但未配置 POST Body".to_string());
                    }
                    if let Some(custom_headers) = self
                        .webhook_custom_headers
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        crate::utils::notification::NotificationClient::validate_custom_webhook_headers(custom_headers)
                            .map_err(|e| format!("Webhook 自定义 Headers 配置无效: {}", e))?;
                    }
                }
                _ => {}
            }
        }

        if self.notification_min_videos == 0 {
            return Err("推送阈值必须大于0".to_string());
        }

        if self.notification_timeout < 5 || self.notification_timeout > 60 {
            return Err("推送超时时间必须在5-60秒之间".to_string());
        }

        if self.notification_retry_count < 1 || self.notification_retry_count > 5 {
            return Err("推送重试次数必须在1-5次之间".to_string());
        }

        Ok(())
    }
}

// 风控验证配置结构体
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RiskControlConfig {
    /// 是否启用风控验证
    #[serde(default)]
    pub enabled: bool,
    /// 验证模式: "manual" (Web界面人工验证), "auto" (自动验证), "skip" (跳过)
    #[serde(default = "default_risk_control_mode")]
    pub mode: String,
    /// 验证等待超时时间（秒）
    #[serde(default = "default_risk_control_timeout")]
    pub timeout: u64,
    /// 自动验证配置
    #[serde(default)]
    pub auto_solve: Option<AutoSolveConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AutoSolveConfig {
    /// 验证码识别服务: "2captcha", "anticaptcha"
    #[serde(default = "default_captcha_service")]
    pub service: String,
    /// API密钥
    pub api_key: String,
    /// 最大重试次数
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// 单次验证超时时间（秒）
    #[serde(default = "default_solve_timeout")]
    pub solve_timeout: u64,
}

fn default_captcha_service() -> String {
    "2captcha".to_string()
}

fn default_max_retries() -> u32 {
    3
}

fn default_solve_timeout() -> u64 {
    120 // 2分钟
}

fn default_risk_control_mode() -> String {
    "manual".to_string() // 默认弹出验证页面进行手动验证
}

fn default_risk_control_timeout() -> u64 {
    300 // 默认5分钟超时
}

impl Default for RiskControlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: default_risk_control_mode(),
            timeout: default_risk_control_timeout(),
            auto_solve: None,
        }
    }
}

impl RiskControlConfig {
    #[allow(dead_code)]
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.mode.as_str(), "manual" | "auto" | "skip") {
            return Err("风控验证模式必须是 'manual', 'auto' 或 'skip'".to_string());
        }

        if self.timeout < 60 || self.timeout > 3600 {
            return Err("验证超时时间必须在60-3600秒之间".to_string());
        }

        // 如果是自动模式，需要验证自动配置
        if self.mode == "auto" {
            if let Some(auto_config) = &self.auto_solve {
                auto_config.validate()?;
            } else {
                return Err("自动验证模式需要配置 auto_solve 参数".to_string());
            }
        }

        Ok(())
    }
}

impl AutoSolveConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.service.as_str(), "2captcha" | "anticaptcha") {
            return Err("验证码识别服务必须是 '2captcha' 或 'anticaptcha'".to_string());
        }

        if self.api_key.is_empty() {
            return Err("API密钥不能为空".to_string());
        }

        if self.max_retries == 0 || self.max_retries > 10 {
            return Err("最大重试次数必须在1-10之间".to_string());
        }

        if self.solve_timeout < 30 || self.solve_timeout > 300 {
            return Err("单次验证超时时间必须在30-300秒之间".to_string());
        }

        Ok(())
    }
}

impl Clone for Config {
    fn clone(&self) -> Self {
        Self {
            auth_token: self.auth_token.clone(),
            bind_address: self.bind_address.clone(),
            credential: ArcSwapOption::from(self.credential.load_full()),
            filter_option: FilterOption {
                video_max_quality: self.filter_option.video_max_quality,
                video_min_quality: self.filter_option.video_min_quality,
                audio_max_quality: self.filter_option.audio_max_quality,
                audio_min_quality: self.filter_option.audio_min_quality,
                codecs: self.filter_option.codecs.clone(),
                no_dolby_video: self.filter_option.no_dolby_video,
                no_dolby_audio: self.filter_option.no_dolby_audio,
                no_hdr: self.filter_option.no_hdr,
                no_hires: self.filter_option.no_hires,
                prefer_douyin_original: self.filter_option.prefer_douyin_original,
            },
            danmaku_option: DanmakuOption {
                duration: self.danmaku_option.duration,
                font: self.danmaku_option.font.clone(),
                font_size: self.danmaku_option.font_size,
                width_ratio: self.danmaku_option.width_ratio,
                horizontal_gap: self.danmaku_option.horizontal_gap,
                lane_size: self.danmaku_option.lane_size,
                float_percentage: self.danmaku_option.float_percentage,
                bottom_percentage: self.danmaku_option.bottom_percentage,
                opacity: self.danmaku_option.opacity,
                bold: self.danmaku_option.bold,
                outline: self.danmaku_option.outline,
                time_offset: self.danmaku_option.time_offset,
            },
            danmaku_update_policy: self.danmaku_update_policy.clone(),
            video_name: self.video_name.clone(),
            page_name: self.page_name.clone(),
            multi_page_name: self.multi_page_name.clone(),
            bangumi_name: self.bangumi_name.clone(),
            folder_structure: self.folder_structure.clone(),
            bangumi_folder_name: self.bangumi_folder_name.clone(),
            collection_folder_mode: self.collection_folder_mode.clone(),
            collection_unified_name: self.collection_unified_name.clone(),
            interval: self.interval,
            upper_path: self.upper_path.clone(),
            favorite_quick_subscribe_path: self.favorite_quick_subscribe_path.clone(),
            collection_quick_subscribe_path: self.collection_quick_subscribe_path.clone(),
            submission_quick_subscribe_path: self.submission_quick_subscribe_path.clone(),
            bangumi_quick_subscribe_path: self.bangumi_quick_subscribe_path.clone(),
            pugv_quick_subscribe_path: self.pugv_quick_subscribe_path.clone(),
            manga_quick_subscribe_path: self.manga_quick_subscribe_path.clone(),
            ffmpeg_path: self.ffmpeg_path.clone(),
            split_chapters_after_download: self.split_chapters_after_download,
            proxy: self.proxy.clone(),
            youtube_proxy: self.youtube_proxy.clone(),
            nfo_time_type: self.nfo_time_type.clone(),
            nfo_config: self.nfo_config.clone(),
            concurrent_limit: self.concurrent_limit.clone(),
            time_format: self.time_format.clone(),
            cdn_sorting: self.cdn_sorting,
            submission_risk_control: self.submission_risk_control.clone(),
            submission_scan_strategy: self.submission_scan_strategy.clone(),
            scan_deleted_videos: self.scan_deleted_videos,
            skip_bangumi_preview: self.skip_bangumi_preview,
            enable_aria2_health_check: self.enable_aria2_health_check,
            enable_aria2_auto_restart: self.enable_aria2_auto_restart,
            aria2_health_check_interval: self.aria2_health_check_interval,
            actors_field_initialized: self.actors_field_initialized,
            multi_page_use_season_structure: self.multi_page_use_season_structure,
            collection_use_season_structure: self.collection_use_season_structure,
            bangumi_use_season_structure: self.bangumi_use_season_structure,
            auto_reorganize_pages: self.auto_reorganize_pages,
            filename_strict_cleaning: self.filename_strict_cleaning,
            download_staging_dir: self.download_staging_dir.clone(),
            download_staging_previous_dirs: self.download_staging_previous_dirs.clone(),
            download_staging_auto_dirs: self.download_staging_auto_dirs.clone(),
            notification: self.notification.clone(),
            enable_startup_data_fix: self.enable_startup_data_fix,
            enable_cid_population: self.enable_cid_population,
            risk_control: self.risk_control.clone(),
            ai_rename: self.ai_rename.clone(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auth_token: None,
            bind_address: default_bind_address(),
            credential: ArcSwapOption::from(Some(Arc::new(Credential::default()))),
            filter_option: FilterOption::default(),
            danmaku_option: DanmakuOption::default(),
            danmaku_update_policy: DanmakuUpdatePolicy::default(),
            video_name: Cow::Borrowed("{{upper_name}}/{{title}}"),
            page_name: Cow::Borrowed("{{pubtime}}-{{bvid}}"),
            multi_page_name: Cow::Borrowed("P{{pid_pad}}.{{ptitle}}"),
            bangumi_name: Cow::Borrowed("S{{season_pad}}E{{pid_pad}}"),
            folder_structure: Cow::Borrowed("Season {{season_pad}}"),
            bangumi_folder_name: Cow::Borrowed("{{series_title}}"),
            collection_folder_mode: Cow::Borrowed("unified"),
            collection_unified_name: default_collection_unified_name(),
            interval: 1200,
            upper_path: CONFIG_DIR.join("upper_face"),
            favorite_quick_subscribe_path: default_quick_subscribe_path_template(),
            collection_quick_subscribe_path: default_quick_subscribe_path_template(),
            submission_quick_subscribe_path: default_quick_subscribe_path_template(),
            bangumi_quick_subscribe_path: default_quick_subscribe_path_template(),
            pugv_quick_subscribe_path: default_quick_subscribe_path_template(),
            manga_quick_subscribe_path: default_quick_subscribe_path_template(),
            ffmpeg_path: default_ffmpeg_path(),
            split_chapters_after_download: default_split_chapters_after_download(),
            proxy: String::new(),
            youtube_proxy: String::new(),
            nfo_time_type: NFOTimeType::FavTime,
            nfo_config: NFOConfig::default(),
            concurrent_limit: ConcurrentLimit::default(),
            time_format: default_time_format(),
            cdn_sorting: default_cdn_sorting(),
            submission_risk_control: crate::config::item::SubmissionRiskControlConfig::default(),
            submission_scan_strategy: SubmissionScanStrategyConfig::default(),
            scan_deleted_videos: false,
            skip_bangumi_preview: default_skip_bangumi_preview(),
            enable_aria2_health_check: false,
            enable_aria2_auto_restart: false,
            aria2_health_check_interval: default_aria2_health_check_interval(),
            actors_field_initialized: false,
            multi_page_use_season_structure: default_multi_page_use_season_structure(),
            collection_use_season_structure: default_collection_use_season_structure(),
            bangumi_use_season_structure: default_bangumi_use_season_structure(),
            auto_reorganize_pages: default_auto_reorganize_pages(),
            filename_strict_cleaning: default_filename_strict_cleaning(),
            download_staging_dir: None,
            download_staging_previous_dirs: Vec::new(),
            download_staging_auto_dirs: Vec::new(),
            notification: NotificationConfig::default(),
            enable_startup_data_fix: false, // 默认关闭，减少不必要的日志
            enable_cid_population: false,   // 默认关闭，减少不必要的日志
            risk_control: RiskControlConfig::default(),
            ai_rename: crate::utils::ai_rename::AiRenameConfig::default(),
        }
    }
}

/// 历史下载暂存目录最多保留的数量，避免配置无限增长。
pub const MAX_DOWNLOAD_STAGING_PREVIOUS_DIRS: usize = 5;

/// 清理历史下载暂存目录列表：只保留绝对路径、按出现顺序去重、剔除与当前暂存目录重复的项，
/// 并限制最多 [`MAX_DOWNLOAD_STAGING_PREVIOUS_DIRS`] 个。
pub fn sanitize_download_staging_previous_dirs(previous_dirs: &mut Vec<PathBuf>, current: Option<&Path>) {
    let mut sanitized: Vec<PathBuf> = Vec::with_capacity(previous_dirs.len());
    for dir in previous_dirs.drain(..) {
        if !dir.is_absolute() {
            continue;
        }
        if current.is_some_and(|current| current == dir.as_path()) {
            continue;
        }
        if sanitized.contains(&dir) {
            continue;
        }
        sanitized.push(dir);
        if sanitized.len() == MAX_DOWNLOAD_STAGING_PREVIOUS_DIRS {
            break;
        }
    }
    *previous_dirs = sanitized;
}

/// 记录被替换下来的旧暂存目录：只记绝对路径、去重（已存在时移到最新位置），
/// 最多保留最近 [`MAX_DOWNLOAD_STAGING_PREVIOUS_DIRS`] 个。
pub fn remember_previous_download_staging_dir(previous_dirs: &mut Vec<PathBuf>, previous: PathBuf) {
    if !previous.is_absolute() {
        return;
    }
    previous_dirs.retain(|dir| dir != &previous);
    previous_dirs.push(previous);
    let overflow = previous_dirs.len().saturating_sub(MAX_DOWNLOAD_STAGING_PREVIOUS_DIRS);
    if overflow > 0 {
        previous_dirs.drain(..overflow);
    }
}

/// 自动下载暂存目录最多保留的数量，避免配置无限增长。
pub const MAX_DOWNLOAD_STAGING_AUTO_DIRS: usize = 16;

/// 清理自动下载暂存目录列表：只保留绝对路径、去重，并限制个数。
pub fn sanitize_download_staging_auto_dirs(auto_dirs: &mut Vec<PathBuf>) {
    let mut sanitized: Vec<PathBuf> = Vec::with_capacity(auto_dirs.len());
    for dir in auto_dirs.drain(..) {
        if !dir.is_absolute() || sanitized.contains(&dir) {
            continue;
        }
        sanitized.push(dir);
        if sanitized.len() == MAX_DOWNLOAD_STAGING_AUTO_DIRS {
            break;
        }
    }
    *auto_dirs = sanitized;
}

/// 记录自动派生并使用过的下载暂存目录：只记绝对路径、去重（已存在时移到最新位置）。
pub fn remember_download_staging_auto_dir(auto_dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if !dir.is_absolute() {
        return;
    }
    auto_dirs.retain(|existing| existing != &dir);
    auto_dirs.push(dir);
    let overflow = auto_dirs.len().saturating_sub(MAX_DOWNLOAD_STAGING_AUTO_DIRS);
    if overflow > 0 {
        auto_dirs.drain(..overflow);
    }
}

impl Config {
    /// 加载配置时清理非法值：下载暂存目录必须是绝对路径，否则回退为「未配置」。
    ///
    /// 相对路径会按进程工作目录解析，可能把下载中间文件写到容器工作目录或错误磁盘，
    /// 因此这里直接丢弃并打 WARN，让下载回到「与媒体目录相同」的旧行为。
    pub fn sanitize_download_staging_dir(&mut self) {
        if let Some(staging_dir) = self.download_staging_dir.as_ref() {
            if !staging_dir.is_absolute() {
                warn!(
                    "下载暂存目录配置无效（必须是绝对路径），已忽略并回退为不使用暂存目录: {}",
                    staging_dir.display()
                );
                self.download_staging_dir = None;
            }
        }
        sanitize_download_staging_previous_dirs(
            &mut self.download_staging_previous_dirs,
            self.download_staging_dir.as_deref(),
        );
        sanitize_download_staging_auto_dirs(&mut self.download_staging_auto_dirs);
    }

    #[cfg(not(test))]
    pub fn check(&self) -> bool {
        let mut ok = true;
        let mut critical_error = false;

        // 移除对视频源的检查，因为现在视频源存储在数据库中
        // let video_sources = self.as_video_sources();
        // if video_sources.is_empty() && self.bangumi.is_empty() {
        //     ok = false;
        //     // 移除错误日志
        //     // error!("没有配置任何需要扫描的内容，程序空转没有意义");
        // }
        // for (args, path) in video_sources {
        //     if !path.is_absolute() {
        //         ok = false;
        //         error!("{:?} 保存的路径应为绝对路径，检测到: {}", args, path.display());
        //     }
        // }
        // // 检查番剧配置的路径
        // for bangumi in &self.bangumi {
        //     if !bangumi.path.is_absolute() {
        //         ok = false;
        //         let season_id_display = match &bangumi.season_id {
        //             Some(id) => id.clone(),
        //             None => "未知".to_string(),
        //         };
        //         error!(
        //             "番剧 {} 保存的路径应为绝对路径，检测到: {}",
        //             season_id_display,
        //             bangumi.path.display()
        //         );
        //     }
        // }

        if !self.upper_path.is_absolute() {
            ok = false;
            error!("up 主头像保存的路径应为绝对路径");
        }
        if let Some(staging_dir) = &self.download_staging_dir {
            if !staging_dir.is_absolute() {
                ok = false;
                error!("下载暂存目录应为绝对路径，检测到: {}", staging_dir.display());
            }
        }
        if self.video_name.is_empty() {
            ok = false;
            error!("未设置 video_name 模板");
        }
        if self.page_name.is_empty() {
            ok = false;
            error!("未设置 page_name 模板");
        }
        if self.multi_page_name.is_empty() {
            ok = false;
            error!("未设置 multi_page_name 模板");
        }
        if self.bangumi_name.is_empty() {
            ok = false;
            error!("未设置 bangumi_name 模板");
        }
        if self.folder_structure.is_empty() {
            ok = false;
            error!("未设置 folder_structure 模板");
        }
        let credential = self.credential.load();
        match credential.as_deref() {
            Some(credential) => {
                if credential.sessdata.is_empty()
                    || credential.bili_jct.is_empty()
                    || credential.buvid3.is_empty()
                    || credential.dedeuserid.is_empty()
                    || credential.ac_time_value.is_empty()
                {
                    ok = false;
                    critical_error = true;
                    warn!("未设置完整的B站登录凭证，程序将以受限模式运行");
                }
            }
            None => {
                ok = false;
                critical_error = true;
                warn!("未设置B站登录凭证，程序将以受限模式运行");
            }
        }
        if !(self.concurrent_limit.video > 0 && self.concurrent_limit.page > 0) {
            ok = false;
            error!("video 和 page 允许的并发数必须大于 0");
        }

        if let Err(err) = self.danmaku_update_policy.validate() {
            ok = false;
            error!("弹幕增量更新策略无效：{}", err);
        }

        if critical_error {
            warn!("配置中检测到凭证未设置，程序将继续运行但功能受限");
            warn!("请通过Web管理界面添加B站登录凭证以启用完整功能");
            // 不再使用 panic!，而是允许程序继续运行
        }

        if let Err(e) = self.notification.validate() {
            ok = false;
            warn!("通知配置无效：{}", e);
        }

        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn sanitize_download_staging_dir_drops_relative_path() {
        let mut config = Config {
            download_staging_dir: Some(PathBuf::from("rel/stage")),
            ..Default::default()
        };
        config.sanitize_download_staging_dir();
        assert_eq!(config.download_staging_dir, None);
    }

    #[test]
    fn sanitize_download_staging_dir_keeps_absolute_path() {
        let mut config = Config {
            download_staging_dir: Some(PathBuf::from("/mnt/staging")),
            ..Default::default()
        };
        config.sanitize_download_staging_dir();
        assert_eq!(config.download_staging_dir.as_deref(), Some(Path::new("/mnt/staging")));
    }

    #[test]
    fn sanitize_download_staging_previous_dirs_drops_relative_duplicates_and_current() {
        let mut previous = vec![
            PathBuf::from("/mnt/old-a"),
            PathBuf::from("rel/stage"),
            PathBuf::from("/mnt/current"),
            PathBuf::from("/mnt/old-a"),
            PathBuf::from("/mnt/old-b"),
        ];
        sanitize_download_staging_previous_dirs(&mut previous, Some(Path::new("/mnt/current")));
        assert_eq!(previous, vec![PathBuf::from("/mnt/old-a"), PathBuf::from("/mnt/old-b")]);
    }

    #[test]
    fn remember_previous_download_staging_dir_keeps_recent_five() {
        let mut previous = Vec::new();
        for index in 0..7 {
            remember_previous_download_staging_dir(&mut previous, PathBuf::from(format!("/mnt/staging-{index}")));
        }
        assert_eq!(previous.len(), MAX_DOWNLOAD_STAGING_PREVIOUS_DIRS);
        assert_eq!(
            previous.first().map(PathBuf::as_path),
            Some(Path::new("/mnt/staging-2"))
        );
        assert_eq!(previous.last().map(PathBuf::as_path), Some(Path::new("/mnt/staging-6")));
    }

    #[test]
    fn remember_previous_download_staging_dir_ignores_relative_paths() {
        let mut previous = vec![PathBuf::from("/mnt/old")];
        remember_previous_download_staging_dir(&mut previous, PathBuf::from("rel/stage"));
        assert_eq!(previous, vec![PathBuf::from("/mnt/old")]);
    }

    #[test]
    fn remember_download_staging_auto_dir_dedupes_and_keeps_recent_sixteen() {
        let mut auto_dirs = Vec::new();
        for index in 0..20 {
            remember_download_staging_auto_dir(&mut auto_dirs, PathBuf::from(format!("/mnt/auto-{index}")));
        }
        assert_eq!(auto_dirs.len(), MAX_DOWNLOAD_STAGING_AUTO_DIRS);
        assert_eq!(auto_dirs.first().map(PathBuf::as_path), Some(Path::new("/mnt/auto-4")));
        assert_eq!(auto_dirs.last().map(PathBuf::as_path), Some(Path::new("/mnt/auto-19")));
        remember_download_staging_auto_dir(&mut auto_dirs, PathBuf::from("/mnt/auto-4"));
        assert_eq!(auto_dirs.last().map(PathBuf::as_path), Some(Path::new("/mnt/auto-4")));
        assert_eq!(auto_dirs.len(), MAX_DOWNLOAD_STAGING_AUTO_DIRS);
    }

    #[test]
    fn sanitize_download_staging_auto_dirs_drops_relative_and_duplicates() {
        let mut auto_dirs = vec![
            PathBuf::from("/mnt/a"),
            PathBuf::from("rel/auto"),
            PathBuf::from("/mnt/a"),
            PathBuf::from("/mnt/b"),
        ];
        sanitize_download_staging_auto_dirs(&mut auto_dirs);
        assert_eq!(auto_dirs, vec![PathBuf::from("/mnt/a"), PathBuf::from("/mnt/b")]);
    }

    #[test]
    fn config_default_has_no_auto_staging_dirs() {
        let config = Config::default();
        assert!(config.download_staging_auto_dirs.is_empty());
    }

    #[test]
    fn filename_strict_cleaning_defaults_to_enabled() {
        assert!(Config::default().filename_strict_cleaning);
    }
}
