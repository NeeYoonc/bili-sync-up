use serde::Deserialize;
use utoipa::IntoParams;
use utoipa::ToSchema;

#[derive(Deserialize, IntoParams, Default)]
pub struct VideosRequest {
    pub collection: Option<i32>,
    pub favorite: Option<i32>,
    pub submission: Option<i32>,
    pub watch_later: Option<i32>,
    pub bangumi: Option<i32>,
    pub query: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub show_failed_only: Option<bool>,
    pub min_height: Option<u32>,
    pub max_height: Option<u32>,
    pub resolution: Option<u32>,
    pub force: Option<bool>,
    pub sort_by: Option<String>,    // "id", "name", "upper_name", "created_at", "updated_at"
    pub sort_order: Option<String>, // "asc", "desc"
}

#[derive(Deserialize, IntoParams)]
pub struct SubmissionVideosRequest {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub keyword: Option<String>, // 搜索关键词
}

// 添加新视频源的请求结构体
#[derive(Deserialize, IntoParams, ToSchema)]
pub struct AddVideoSourceRequest {
    // 视频源类型: "collection", "favorite", "submission", "watch_later", "bangumi"
    pub source_type: String,
    // 视频源ID: 收藏夹ID、合集ID、UP主ID等
    pub source_id: String,
    // UP主ID: 仅当source_type为"collection"时需要
    pub up_id: Option<String>,
    // 视频源名称
    pub name: String,
    // 保存路径
    pub path: String,
    // 合集类型: "season"(视频合集) 或 "series"(视频列表)，仅当source_type为"collection"时有效
    pub collection_type: Option<String>,
    // 番剧特有字段
    pub media_id: Option<String>,
    pub ep_id: Option<String>,
    // 是否下载全部季度，仅当source_type为"bangumi"时有效
    pub download_all_seasons: Option<bool>,
    // 选中的季度ID列表，仅当source_type为"bangumi"且download_all_seasons为false时有效
    pub selected_seasons: Option<Vec<String>>,
    // 选中的视频ID列表，仅当source_type为"submission"时有效，用于选择性下载历史投稿
    pub selected_videos: Option<Vec<String>>,
    // 封面URL，仅当source_type为"collection"时有效
    pub cover: Option<String>,
    // 合并到现有番剧源的ID，仅当source_type为"bangumi"时有效
    pub merge_to_source_id: Option<i32>,
    // 关键词过滤器列表（支持正则表达式）
    pub keyword_filters: Option<Vec<String>>,
    // 关键词过滤模式: "blacklist"（黑名单-排除匹配）或 "whitelist"（白名单-只下载匹配）
    pub keyword_filter_mode: Option<String>,
    // 是否仅下载音频（输出为m4a文件）
    pub audio_only: Option<bool>,
    // 是否下载弹幕文件（ASS）
    pub download_danmaku: Option<bool>,
    // 是否下载字幕文件（SRT）
    pub download_subtitle: Option<bool>,
    // 是否启用AI重命名
    pub ai_rename: Option<bool>,
    // AI重命名视频提示词（覆盖全局配置）
    pub ai_rename_video_prompt: Option<String>,
    // AI重命名音频提示词（覆盖全局配置）
    pub ai_rename_audio_prompt: Option<String>,
    // AI重命名高级选项：对多P视频启用AI重命名
    pub ai_rename_enable_multi_page: Option<bool>,
    // AI重命名高级选项：对合集视频启用AI重命名
    pub ai_rename_enable_collection: Option<bool>,
    // AI重命名高级选项：对番剧启用AI重命名
    pub ai_rename_enable_bangumi: Option<bool>,
    // AI重命名高级选项：是否重命名上级目录
    pub ai_rename_rename_parent_dir: Option<bool>,
    // 仅音频时是否只保留m4a（不下载封面/nfo/弹幕/字幕）
    pub audio_only_m4a_only: Option<bool>,
    // 是否启用平铺目录模式（不为每个视频创建子文件夹）
    pub flat_folder: Option<bool>,
    // 是否使用动态API获取UP主投稿（仅submission有效）
    pub use_dynamic_api: Option<bool>,
}

// 删除视频源的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct DeleteVideoSourceRequest {
    pub delete_local_files: bool,
}

// 更新视频源启用状态的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateVideoSourceEnabledRequest {
    pub enabled: bool,
}

// 更新视频源扫描已删除视频设置的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateVideoSourceScanDeletedRequest {
    pub scan_deleted_videos: bool,
}

// 更新视频源下载选项的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateVideoSourceDownloadOptionsRequest {
    /// 是否仅下载音频（输出为m4a文件）
    pub audio_only: Option<bool>,
    /// 仅音频时是否只保留m4a（不下载封面/nfo/弹幕/字幕）
    pub audio_only_m4a_only: Option<bool>,
    /// 是否启用平铺目录模式（不为每个视频创建子文件夹）
    pub flat_folder: Option<bool>,
    /// 是否下载弹幕文件（ASS）
    pub download_danmaku: Option<bool>,
    /// 是否下载字幕文件（SRT）
    pub download_subtitle: Option<bool>,
    /// 是否启用AI重命名
    pub ai_rename: Option<bool>,
    /// AI重命名视频提示词（覆盖全局配置）
    pub ai_rename_video_prompt: Option<String>,
    /// AI重命名音频提示词（覆盖全局配置）
    pub ai_rename_audio_prompt: Option<String>,
    /// 是否对多P视频启用AI重命名
    pub ai_rename_enable_multi_page: Option<bool>,
    /// 是否对合集视频启用AI重命名
    pub ai_rename_enable_collection: Option<bool>,
    /// 是否对番剧启用AI重命名
    pub ai_rename_enable_bangumi: Option<bool>,
    /// AI重命名时是否重命名上级目录
    pub ai_rename_rename_parent_dir: Option<bool>,
    /// 是否使用动态API获取UP主投稿（仅submission有效）
    pub use_dynamic_api: Option<bool>,
}

// 更新投稿源选中视频列表的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSubmissionSelectedVideosRequest {
    /// 选中的视频BVID列表，用于选择性下载历史投稿
    pub selected_videos: Vec<String>,
}

// 重设视频源路径的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetVideoSourcePathRequest {
    /// 新的基础路径
    pub new_path: String,
    /// 是否应用四步重命名原则移动文件
    #[serde(default = "default_apply_rename_rules")]
    pub apply_rename_rules: bool,
    /// 是否删除空的原始文件夹
    #[serde(default = "default_clean_empty_folders")]
    pub clean_empty_folders: bool,
}

fn default_apply_rename_rules() -> bool {
    true
}

fn default_clean_empty_folders() -> bool {
    true
}

// 更新配置的请求结构体
#[derive(Deserialize, IntoParams, ToSchema)]
pub struct UpdateConfigRequest {
    // 视频命名模板
    pub video_name: Option<String>,
    // 分页命名模板
    pub page_name: Option<String>,
    // 多P视频分页命名模板
    pub multi_page_name: Option<String>,
    // 番剧分页命名模板
    pub bangumi_name: Option<String>,
    // 文件夹结构模板
    pub folder_structure: Option<String>,
    // 番剧文件夹命名模板
    pub bangumi_folder_name: Option<String>,
    // 合集文件夹模式
    pub collection_folder_mode: Option<String>,
    // 合集统一模式命名模板（仅 unified 模式生效）
    pub collection_unified_name: Option<String>,
    // 时间格式
    pub time_format: Option<String>,
    // 扫描间隔（秒）
    pub interval: Option<u64>,
    // NFO时间类型
    pub nfo_time_type: Option<String>,
    // 多线程下载配置
    pub parallel_download_enabled: Option<bool>,
    pub parallel_download_threads: Option<usize>,
    pub parallel_download_use_aria2: Option<bool>,
    // 视频质量设置
    pub video_max_quality: Option<String>,
    pub video_min_quality: Option<String>,
    pub audio_max_quality: Option<String>,
    pub audio_min_quality: Option<String>,
    pub codecs: Option<Vec<String>>,
    pub no_dolby_video: Option<bool>,
    pub no_dolby_audio: Option<bool>,
    pub no_hdr: Option<bool>,
    pub no_hires: Option<bool>,
    // 弹幕设置
    pub danmaku_duration: Option<f64>,
    pub danmaku_font: Option<String>,
    pub danmaku_font_size: Option<u32>,
    pub danmaku_width_ratio: Option<f64>,
    pub danmaku_horizontal_gap: Option<f64>,
    pub danmaku_lane_size: Option<u32>,
    pub danmaku_float_percentage: Option<f64>,
    pub danmaku_bottom_percentage: Option<f64>,
    pub danmaku_opacity: Option<u8>,
    pub danmaku_bold: Option<bool>,
    pub danmaku_outline: Option<f64>,
    pub danmaku_time_offset: Option<f64>,
    // 并发控制设置
    pub concurrent_video: Option<usize>,
    pub concurrent_page: Option<usize>,
    pub rate_limit: Option<usize>,
    pub rate_duration: Option<u64>,
    // 其他设置
    pub cdn_sorting: Option<bool>,
    // UP主投稿风控配置
    pub large_submission_threshold: Option<usize>,
    pub base_request_delay: Option<u64>,
    pub large_submission_delay_multiplier: Option<u64>,
    pub enable_progressive_delay: Option<bool>,
    pub max_delay_multiplier: Option<u64>,
    pub enable_incremental_fetch: Option<bool>,
    pub incremental_fallback_to_full: Option<bool>,
    pub enable_batch_processing: Option<bool>,
    pub batch_size: Option<usize>,
    pub batch_delay_seconds: Option<u64>,
    pub enable_auto_backoff: Option<bool>,
    pub auto_backoff_base_seconds: Option<u64>,
    pub auto_backoff_max_multiplier: Option<u64>,
    pub source_delay_seconds: Option<u64>,
    pub submission_source_delay_seconds: Option<u64>,
    // UP主投稿源扫描策略
    pub submission_scan_batch_size: Option<usize>,
    pub submission_adaptive_scan: Option<bool>,
    pub submission_adaptive_max_hours: Option<u64>,
    // 系统配置
    pub scan_deleted_videos: Option<bool>,
    // aria2监控配置
    pub enable_aria2_health_check: Option<bool>,
    pub enable_aria2_auto_restart: Option<bool>,
    pub aria2_health_check_interval: Option<u64>,
    // 多P视频目录结构配置
    pub multi_page_use_season_structure: Option<bool>,
    // 合集目录结构配置
    pub collection_use_season_structure: Option<bool>,
    // 番剧目录结构配置
    pub bangumi_use_season_structure: Option<bool>,
    // UP主头像保存路径
    pub upper_path: Option<String>,
    // ffmpeg 路径（可填 ffmpeg.exe 文件路径或其所在目录）
    pub ffmpeg_path: Option<String>,
    // 风控验证配置
    pub risk_control_enabled: Option<bool>,
    pub risk_control_mode: Option<String>,
    pub risk_control_timeout: Option<u64>,
    // 自动验证配置
    pub risk_control_auto_solve_service: Option<String>,
    pub risk_control_auto_solve_api_key: Option<String>,
    pub risk_control_auto_solve_max_retries: Option<u32>,
    pub risk_control_auto_solve_timeout: Option<u64>,
    // AI重命名配置
    pub ai_rename_enabled: Option<bool>,
    pub ai_rename_provider: Option<String>,
    pub ai_rename_base_url: Option<String>,
    pub ai_rename_api_key: Option<String>,
    pub ai_rename_deepseek_web_token: Option<String>,
    pub ai_rename_model: Option<String>,
    pub ai_rename_timeout_seconds: Option<u64>,
    pub ai_rename_video_prompt_hint: Option<String>,
    pub ai_rename_audio_prompt_hint: Option<String>,
    pub ai_rename_enable_multi_page: Option<bool>,
    pub ai_rename_enable_collection: Option<bool>,
    pub ai_rename_enable_bangumi: Option<bool>,
    pub ai_rename_rename_parent_dir: Option<bool>,
    // 服务器绑定地址
    pub bind_address: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SearchRequest {
    pub keyword: String,     // 搜索关键词
    pub search_type: String, // 搜索类型：video, bili_user, media_bangumi
    #[serde(default = "default_page")]
    pub page: u32, // 页码，默认1
    #[serde(default = "default_page_size")]
    pub page_size: u32, // 每页数量，默认20
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}

// 状态更新结构
#[derive(Deserialize, ToSchema)]
pub struct StatusUpdate {
    pub status_index: usize, // 状态位索引 (0-4)
    pub status_value: u32,   // 状态值 (0, 1, 2, 3)
}

// 推送配置更新请求
#[derive(Deserialize, ToSchema)]
pub struct UpdateNotificationConfigRequest {
    pub active_channel: Option<String>,
    pub serverchan_key: Option<String>,
    pub serverchan3_uid: Option<String>,
    pub serverchan3_sendkey: Option<String>,
    pub wecom_webhook_url: Option<String>,
    pub wecom_msgtype: Option<String>,
    pub wecom_mention_all: Option<bool>,
    pub wecom_mentioned_list: Option<Vec<String>>,
    pub enable_scan_notifications: Option<bool>,
    pub notification_min_videos: Option<usize>,
    pub notification_timeout: Option<u64>,
    pub notification_retry_count: Option<u8>,
}

// 测试推送请求（可选消息内容）
#[derive(Deserialize, ToSchema)]
pub struct TestNotificationRequest {
    pub custom_message: Option<String>,
}

// 分页状态更新结构
#[derive(Deserialize, ToSchema)]
pub struct PageStatusUpdate {
    pub page_id: i32,
    pub updates: Vec<StatusUpdate>,
}

// 更新视频状态请求
#[derive(Deserialize, ToSchema)]
pub struct UpdateVideoStatusRequest {
    #[serde(default)]
    pub video_updates: Vec<StatusUpdate>,
    #[serde(default)]
    pub page_updates: Vec<PageStatusUpdate>,
}

// 选择性重置任务请求
#[derive(Deserialize, ToSchema)]
pub struct ResetSpecificTasksRequest {
    pub task_indexes: Vec<usize>, // 要重置的任务索引列表 (0-4)
    pub collection: Option<i32>,
    pub favorite: Option<i32>,
    pub submission: Option<i32>,
    pub watch_later: Option<i32>,
    pub bangumi: Option<i32>,
    // 与 /api/videos 的过滤参数保持一致，便于“按当前筛选批量重置”
    pub query: Option<String>,
    pub show_failed_only: Option<bool>,
    pub min_height: Option<u32>,
    pub max_height: Option<u32>,
    pub resolution: Option<u32>,
    // 默认仅重置失败任务；force=true 时重置所有非 0 状态（包括已完成）
    pub force: Option<bool>,
}

// 配置管理相关请求结构体

// 更新单个配置项请求
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateConfigItemRequest {
    pub value: serde_json::Value,
}

// 批量更新配置请求
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BatchUpdateConfigRequest {
    pub items: std::collections::HashMap<String, serde_json::Value>,
}

// 配置历史查询请求
#[derive(Deserialize, IntoParams)]
pub struct ConfigHistoryRequest {
    pub key: Option<String>,
    pub limit: Option<u64>,
}

// 初始设置相关请求

// 设置API Token请求
#[derive(Deserialize, ToSchema)]
pub struct SetupAuthTokenRequest {
    pub auth_token: String,
}

// 更新凭证请求
#[derive(Deserialize, ToSchema)]
pub struct UpdateCredentialRequest {
    pub sessdata: String,
    pub bili_jct: String,
    pub buvid3: String,
    pub dedeuserid: String,
    pub ac_time_value: Option<String>,
    pub buvid4: Option<String>,
    pub dedeuserid_ckmd5: Option<String>,
}

// 扫码登录相关请求

// 生成二维码请求
#[derive(Deserialize, ToSchema)]
pub struct QRGenerateRequest {}

// 轮询二维码状态请求
#[derive(Deserialize, IntoParams)]
pub struct QRPollRequest {
    pub session_id: String,
}

// 更新关键词过滤器的请求结构体（支持双列表模式）
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateKeywordFiltersRequest {
    /// 黑名单关键词列表（支持正则表达式）
    /// 匹配黑名单的视频将被排除（即使通过了白名单）
    pub blacklist_keywords: Option<Vec<String>>,
    /// 白名单关键词列表（支持正则表达式）
    /// 如果设置了白名单，视频必须匹配其中之一才会被下载
    pub whitelist_keywords: Option<Vec<String>>,
    /// 是否区分大小写（默认为 true）
    #[serde(default = "default_case_sensitive")]
    pub case_sensitive: Option<bool>,
    /// 【已废弃】关键词列表（支持正则表达式）- 向后兼容
    #[serde(default)]
    pub keyword_filters: Option<Vec<String>>,
    /// 【已废弃】关键词过滤模式 - 向后兼容
    #[serde(default)]
    pub keyword_filter_mode: Option<String>,
}

fn default_case_sensitive() -> Option<bool> {
    None
}

// 验证正则表达式的请求结构体
#[derive(Debug, Deserialize, ToSchema)]
pub struct ValidateRegexRequest {
    /// 要验证的正则表达式
    pub pattern: String,
}

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct ConfigMigrationRequest {
    pub dry_run: Option<bool>,
}
