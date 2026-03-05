use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::utils::filenamify::filenamify;

/// NFO 文件使用的时间类型
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum NFOTimeType {
    #[default]
    FavTime,
    PubTime,
}

/// NFO 文件格式类型
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum NFOFormatType {
    /// 标准 Kodi 格式（推荐）
    #[default]
    Kodi,
    /// 简化格式（兼容旧版本）
    Simple,
    /// 详细格式（包含所有可用信息）
    Detailed,
}

/// 空UP主信息处理策略
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum EmptyUpperStrategy {
    /// 跳过演员信息（默认策略）
    #[default]
    Skip,
    /// 使用占位符文本
    Placeholder,
    /// 使用默认UP主名称
    Default,
}

/// NFO 生成配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NFOConfig {
    /// 是否生成 NFO 文件
    #[serde(default = "default_nfo_enabled")]
    pub enabled: bool,
    /// NFO 文件格式类型
    #[serde(default)]
    pub format_type: NFOFormatType,
    /// NFO 文件使用的时间类型
    #[serde(default)]
    pub time_type: NFOTimeType,
    /// 是否包含B站特有信息（如播放量、点赞数等）
    #[serde(default = "default_include_bilibili_info")]
    pub include_bilibili_info: bool,
    /// 是否生成演员信息（UP主作为创作者）
    #[serde(default = "default_include_actor_info")]
    pub include_actor_info: bool,
    /// 默认国家（当视频信息中没有时使用）
    #[serde(default = "default_default_country")]
    pub default_country: String,
    /// 默认制作公司（当视频信息中没有时使用）
    #[serde(default = "default_default_studio")]
    pub default_studio: String,
    /// 番剧默认播出状态
    #[serde(default = "default_tvshow_status")]
    pub default_tvshow_status: String,
    /// 空UP主信息处理策略
    #[serde(default)]
    pub empty_upper_strategy: EmptyUpperStrategy,
    /// 空UP主的占位符文本（当策略为Placeholder时使用）
    #[serde(default = "default_empty_upper_placeholder")]
    pub empty_upper_placeholder: String,
    /// 空UP主的默认名称（当策略为Default时使用）
    #[serde(default = "default_empty_upper_default_name")]
    pub empty_upper_default_name: String,
}

fn default_nfo_enabled() -> bool {
    true
}

fn default_include_bilibili_info() -> bool {
    true
}

fn default_include_actor_info() -> bool {
    true
}

fn default_default_country() -> String {
    "中国".to_string()
}

fn default_default_studio() -> String {
    "哔哩哔哩".to_string()
}

fn default_tvshow_status() -> String {
    "Continuing".to_string()
}

fn default_empty_upper_placeholder() -> String {
    "官方内容".to_string()
}

fn default_empty_upper_default_name() -> String {
    "哔哩哔哩".to_string()
}

impl Default for NFOConfig {
    fn default() -> Self {
        Self {
            enabled: default_nfo_enabled(),
            format_type: NFOFormatType::default(),
            time_type: NFOTimeType::default(),
            include_bilibili_info: default_include_bilibili_info(),
            include_actor_info: default_include_actor_info(),
            default_country: default_default_country(),
            default_studio: default_default_studio(),
            default_tvshow_status: default_tvshow_status(),
            empty_upper_strategy: EmptyUpperStrategy::default(),
            empty_upper_placeholder: default_empty_upper_placeholder(),
            empty_upper_default_name: default_empty_upper_default_name(),
        }
    }
}

/// 多线程下载配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ParallelDownloadConfig {
    /// 是否启用多线程下载
    #[serde(default = "default_parallel_download_enabled")]
    pub enabled: bool,
    /// 每个文件的下载线程数
    #[serde(default = "default_parallel_download_threads")]
    pub threads: usize,
    /// 是否优先使用 aria2（关闭后将使用原生多线程分片下载）
    #[serde(default = "default_parallel_download_use_aria2")]
    pub use_aria2: bool,
}

fn default_parallel_download_enabled() -> bool {
    true
}

fn default_parallel_download_threads() -> usize {
    4
}

fn default_parallel_download_use_aria2() -> bool {
    false
}

impl Default for ParallelDownloadConfig {
    fn default() -> Self {
        Self {
            enabled: default_parallel_download_enabled(),
            threads: default_parallel_download_threads(),
            use_aria2: default_parallel_download_use_aria2(),
        }
    }
}

/// 并发下载相关的配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConcurrentLimit {
    pub video: usize,
    pub page: usize,
    pub rate_limit: Option<RateLimit>,
    #[serde(default)]
    pub parallel_download: ParallelDownloadConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RateLimit {
    pub limit: usize,
    pub duration: u64,
}

impl Default for ConcurrentLimit {
    fn default() -> Self {
        Self {
            video: 3,
            page: 2,
            // 默认的限速配置，每 250ms 允许请求 4 次
            rate_limit: Some(RateLimit {
                limit: 4,
                duration: 250,
            }),
            parallel_download: ParallelDownloadConfig::default(),
        }
    }
}

/// UP主投稿风控配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubmissionRiskControlConfig {
    /// 大量视频UP主的阈值（超过此数量视为大量视频UP主）
    #[serde(default = "default_large_submission_threshold")]
    pub large_submission_threshold: usize,
    /// 基础请求间隔（毫秒）
    #[serde(default = "default_base_request_delay")]
    pub base_request_delay: u64,
    /// 大量视频UP主的额外延迟倍数
    #[serde(default = "default_large_submission_delay_multiplier")]
    pub large_submission_delay_multiplier: u64,
    /// 是否启用渐进式延迟（请求次数越多，延迟越长）
    #[serde(default = "default_enable_progressive_delay")]
    pub enable_progressive_delay: bool,
    /// 渐进式延迟的最大倍数
    #[serde(default = "default_max_delay_multiplier")]
    pub max_delay_multiplier: u64,
    /// 是否启用增量获取（只获取比上次扫描更新的视频）
    #[serde(default = "default_enable_incremental_fetch")]
    pub enable_incremental_fetch: bool,
    /// 增量获取失败时是否回退到全量获取
    #[serde(default = "default_incremental_fallback_to_full")]
    pub incremental_fallback_to_full: bool,
    /// 是否启用分批处理（大量视频的UP主分批请求）
    #[serde(default = "default_enable_batch_processing")]
    pub enable_batch_processing: bool,
    /// 分批处理的批次大小（每批处理的页数）
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// 批次间延迟（秒）
    #[serde(default = "default_batch_delay_seconds")]
    pub batch_delay_seconds: u64,
    /// 是否启用自动退避机制（检测到风控时自动增加延迟）
    #[serde(default = "default_enable_auto_backoff")]
    pub enable_auto_backoff: bool,
    /// 自动退避的基础时间（秒）
    #[serde(default = "default_auto_backoff_base_seconds")]
    pub auto_backoff_base_seconds: u64,
    /// 自动退避的最大倍数
    #[serde(default = "default_auto_backoff_max_multiplier")]
    pub auto_backoff_max_multiplier: u64,
    /// 视频源之间的延迟（秒）
    #[serde(default = "default_source_delay_seconds")]
    pub source_delay_seconds: u64,
    /// UP主投稿源之间的特殊延迟（秒）
    #[serde(default = "default_submission_source_delay_seconds")]
    pub submission_source_delay_seconds: u64,
    /// 是否对动态API启用延迟机制（默认启用）
    #[serde(default = "default_enable_dynamic_api_delay")]
    pub enable_dynamic_api_delay: bool,
    /// 动态API的延迟倍数（相对于base_request_delay，默认1.5倍）
    #[serde(default = "default_dynamic_api_delay_multiplier")]
    pub dynamic_api_delay_multiplier: f64,
}

/// UP主投稿源扫描优化配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubmissionScanStrategyConfig {
    /// 每轮最多扫描的投稿源数量；0 表示不限制（保持旧行为）
    #[serde(default = "default_submission_scan_batch_size")]
    pub batch_size: usize,
    /// 是否启用自适应扫描频率（无更新的源自动降频）
    #[serde(default)]
    pub adaptive_enabled: bool,
    /// 自适应扫描的最大间隔（小时）
    #[serde(default = "default_submission_adaptive_max_hours")]
    pub adaptive_max_hours: u64,
}

fn default_submission_scan_batch_size() -> usize {
    0
}

fn default_submission_adaptive_max_hours() -> u64 {
    24
}

impl Default for SubmissionScanStrategyConfig {
    fn default() -> Self {
        Self {
            batch_size: default_submission_scan_batch_size(),
            adaptive_enabled: false,
            adaptive_max_hours: default_submission_adaptive_max_hours(),
        }
    }
}

fn default_large_submission_threshold() -> usize {
    80
}

fn default_base_request_delay() -> u64 {
    1000
}

fn default_large_submission_delay_multiplier() -> u64 {
    2
}

fn default_enable_progressive_delay() -> bool {
    true
}

fn default_max_delay_multiplier() -> u64 {
    4
}

fn default_enable_incremental_fetch() -> bool {
    true
}

fn default_incremental_fallback_to_full() -> bool {
    true
}

fn default_enable_batch_processing() -> bool {
    true // 默认启用分批处理
}

fn default_batch_size() -> usize {
    3 // 每批3页，约90个视频
}

fn default_batch_delay_seconds() -> u64 {
    2 // 批次间延迟2秒
}

fn default_enable_auto_backoff() -> bool {
    false // 默认不启用自动退避
}

fn default_auto_backoff_base_seconds() -> u64 {
    10 // 自动退避基础时间10秒
}

fn default_auto_backoff_max_multiplier() -> u64 {
    5 // 最大退避到50秒
}

fn default_source_delay_seconds() -> u64 {
    2 // 视频源之间默认延迟2秒
}

fn default_submission_source_delay_seconds() -> u64 {
    5 // UP主投稿源之间默认延迟5秒
}

fn default_enable_dynamic_api_delay() -> bool {
    true // 默认启用动态API延迟机制
}

fn default_dynamic_api_delay_multiplier() -> f64 {
    1.5 // 动态API默认使用1.5倍延迟（比普通投稿API更保守）
}

impl Default for SubmissionRiskControlConfig {
    fn default() -> Self {
        Self {
            large_submission_threshold: default_large_submission_threshold(),
            base_request_delay: default_base_request_delay(),
            large_submission_delay_multiplier: default_large_submission_delay_multiplier(),
            enable_progressive_delay: default_enable_progressive_delay(),
            max_delay_multiplier: default_max_delay_multiplier(),
            enable_incremental_fetch: default_enable_incremental_fetch(),
            incremental_fallback_to_full: default_incremental_fallback_to_full(),
            enable_batch_processing: default_enable_batch_processing(),
            batch_size: default_batch_size(),
            batch_delay_seconds: default_batch_delay_seconds(),
            enable_auto_backoff: default_enable_auto_backoff(),
            auto_backoff_base_seconds: default_auto_backoff_base_seconds(),
            auto_backoff_max_multiplier: default_auto_backoff_max_multiplier(),
            source_delay_seconds: default_source_delay_seconds(),
            submission_source_delay_seconds: default_submission_source_delay_seconds(),
            enable_dynamic_api_delay: default_enable_dynamic_api_delay(),
            dynamic_api_delay_multiplier: default_dynamic_api_delay_multiplier(),
        }
    }
}

#[allow(dead_code)]
pub trait PathSafeTemplate {
    fn path_safe_register(&mut self, name: &'static str, template: &'static str) -> Result<()>;
    fn path_safe_render(&self, name: &'static str, data: &serde_json::Value) -> Result<String>;
}

/// 通过将模板字符串中的分隔符替换为自定义的字符串，使得模板字符串中的分隔符得以保留
impl PathSafeTemplate for handlebars::Handlebars<'_> {
    fn path_safe_register(&mut self, name: &'static str, template: &'static str) -> Result<()> {
        // 处理连续的路径分隔符，然后区分Unix风格和Windows风格
        let safe_template = template
            .replace("\\\\", "__WIN_SEP__")   // 连续的Windows反斜杠当作一个分隔符
            .replace("//", "__UNIX_SEP__")    // 连续的Unix正斜杠当作一个分隔符
            .replace('/', "__UNIX_SEP__")     // 单个Unix风格正斜杠
            .replace('\\', "__WIN_SEP__"); // 单个Windows风格反斜杠
        Ok(self.register_template_string(name, safe_template)?)
    }

    fn path_safe_render(&self, name: &'static str, data: &serde_json::Value) -> Result<String> {
        let rendered = filenamify(&self.render(name, data)?);
        #[cfg(windows)]
        {
            // Windows系统下：Unix风格转为下划线，Windows风格保持为反斜杠
            Ok(rendered.replace("__UNIX_SEP__", "_").replace("__WIN_SEP__", "\\"))
        }
        #[cfg(not(windows))]
        {
            // 非Windows系统下：Unix风格转为正斜杠，Windows风格转为下划线
            Ok(rendered.replace("__UNIX_SEP__", "/").replace("__WIN_SEP__", "_"))
        }
    }
}
