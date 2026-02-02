use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use serde::{Deserialize, Serialize};

mod bundle;
mod clap;
mod global;
mod item;
mod manager;

use crate::bilibili::{Credential, DanmakuOption, FilterOption};
pub use crate::config::bundle::ConfigBundle;
pub use crate::config::clap::version;
pub use crate::config::global::{
    get_config_manager, init_config_with_database, reload_config, reload_config_bundle, with_config, ARGS,
    CONFIG_BUNDLE, CONFIG_DIR,
};
use crate::config::item::ConcurrentLimit;
pub use crate::config::item::{
    EmptyUpperStrategy, NFOConfig, NFOTimeType, PathSafeTemplate, RateLimit, SubmissionRiskControlConfig,
    SubmissionScanStrategyConfig,
};
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
    "%Y-%m-%d".to_string()
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
    Cow::Borrowed("{{pubtime}}-{{bvid}}-{{truncate title 20}}")
}

fn default_interval() -> u64 {
    1200
}

fn default_upper_path() -> PathBuf {
    CONFIG_DIR.join("upper_face")
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
    Cow::Borrowed("S01E{{episode_pad}}{{#if is_multi_page}}P{{pid_pad}}{{/if}} - {{title}}")
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
    /// UP主投稿源扫描策略（分批/自适应）
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

fn default_cdn_sorting() -> bool {
    true // 默认启用CDN排序
}

// 推送通知配置结构体
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NotificationConfig {
    // === 当前激活的通知渠道 ===
    #[serde(default = "default_active_channel")]
    pub active_channel: String, // "none", "serverchan", "serverchan3", "wecom"

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
    }

    pub fn validate(&self) -> Result<(), String> {
        // 验证 active_channel 的有效性
        if !["none", "serverchan", "serverchan3", "wecom"].contains(&self.active_channel.as_str()) {
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
            video_name: Cow::Borrowed("{{upper_name}}/{{title}}"),
            page_name: Cow::Borrowed("{{pubtime}}-{{bvid}}-{{truncate title 20}}"),
            multi_page_name: Cow::Borrowed("P{{pid_pad}}.{{ptitle}}"),
            bangumi_name: Cow::Borrowed("S{{season_pad}}E{{pid_pad}}"),
            folder_structure: Cow::Borrowed("Season {{season_pad}}"),
            bangumi_folder_name: Cow::Borrowed("{{series_title}}"),
            collection_folder_mode: Cow::Borrowed("unified"),
            collection_unified_name: default_collection_unified_name(),
            interval: 1200,
            upper_path: CONFIG_DIR.join("upper_face"),
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
            notification: NotificationConfig::default(),
            enable_startup_data_fix: false, // 默认关闭，减少不必要的日志
            enable_cid_population: false,   // 默认关闭，减少不必要的日志
            risk_control: RiskControlConfig::default(),
            ai_rename: crate::utils::ai_rename::AiRenameConfig::default(),
        }
    }
}

impl Config {
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
