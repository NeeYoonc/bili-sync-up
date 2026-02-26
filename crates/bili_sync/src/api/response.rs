use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::utils::status::{PageStatus, VideoStatus};

#[derive(Debug, Serialize, ToSchema, Default)]
pub struct VideoSourcesResponse {
    #[serde(default)]
    pub collection: Vec<VideoSource>,
    #[serde(default)]
    pub favorite: Vec<VideoSource>,
    #[serde(default)]
    pub submission: Vec<VideoSource>,
    #[serde(default)]
    pub watch_later: Vec<VideoSource>,
    #[serde(default)]
    pub bangumi: Vec<VideoSource>,
}

#[derive(Serialize, ToSchema)]
pub struct VideosResponse {
    pub videos: Vec<VideoInfo>,
    pub total_count: u64,
}

#[derive(Serialize, ToSchema)]
pub struct VideoResponse {
    pub video: VideoInfo,
    pub pages: Vec<PageInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct VideoBvidResponse {
    pub bvid: String,
    pub title: String,
    pub bilibili_url: String,
}

#[derive(Serialize, ToSchema)]
pub struct ResetVideoResponse {
    pub resetted: bool,
    pub video: VideoInfo,
    pub pages: Vec<PageInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateVideoStatusResponse {
    pub success: bool,
    pub video: VideoInfo,
    pub pages: Vec<PageInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct ResetAllVideosResponse {
    pub resetted: bool,
    pub resetted_videos_count: usize,
    pub resetted_pages_count: usize,
}

#[derive(Serialize, ToSchema)]
pub struct AddVideoSourceResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct SubmissionVideosResponse {
    pub videos: Vec<SubmissionVideoInfo>,
    pub total: i64,
    pub page: i32,
    pub page_size: i32,
}

#[derive(Serialize, ToSchema)]
pub struct SubmissionVideoInfo {
    pub bvid: String,
    pub title: String,
    pub cover: String,
    pub pubtime: String,
    pub duration: i32,
    pub view: i32,
    pub danmaku: i32,
    pub description: String,
}

#[derive(Serialize, ToSchema)]
pub struct DeleteVideoSourceResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct DeleteVideoResponse {
    pub success: bool,
    pub video_id: i32,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateVideoSourceEnabledResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub enabled: bool,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateVideoSourceScanDeletedResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub scan_deleted_videos: bool,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateVideoSourceDownloadOptionsResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub audio_only: bool,
    pub audio_only_m4a_only: bool,
    pub flat_folder: bool,
    pub download_danmaku: bool,
    pub download_subtitle: bool,
    pub ai_rename: bool,
    pub ai_rename_video_prompt: String,
    pub ai_rename_audio_prompt: String,
    pub ai_rename_enable_multi_page: bool,
    pub ai_rename_enable_collection: bool,
    pub ai_rename_enable_bangumi: bool,
    pub ai_rename_rename_parent_dir: bool,
    pub use_dynamic_api: bool,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateSubmissionSelectedVideosResponse {
    pub success: bool,
    pub source_id: i32,
    pub selected_count: usize,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct ResetVideoSourcePathResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub old_path: String,
    pub new_path: String,
    pub moved_files_count: usize,
    pub updated_videos_count: usize,
    pub cleaned_folders_count: usize,
    pub message: String,
}

/// 番剧源简化信息（用于合并选择）
#[derive(Serialize, ToSchema, Debug)]
pub struct BangumiSourceOption {
    pub id: i32,
    pub name: String,
    pub path: String,
    pub season_id: Option<String>,
    pub media_id: Option<String>,
    pub download_all_seasons: bool,
    pub selected_seasons_count: usize,
}

/// 番剧源列表响应
#[derive(Serialize, ToSchema)]
pub struct BangumiSourceListResponse {
    pub success: bool,
    pub bangumi_sources: Vec<BangumiSourceOption>,
    pub total_count: usize,
}

#[derive(Serialize, ToSchema, Debug)]
pub struct VideoSource {
    pub id: i32,
    pub name: String,
    pub enabled: bool,
    pub path: String,
    pub scan_deleted_videos: bool,
    // 类型特有的ID字段
    pub f_id: Option<i64>,         // 收藏夹ID
    pub s_id: Option<i64>,         // 合集ID
    pub m_id: Option<i64>,         // UP主ID (用于合集)
    pub upper_id: Option<i64>,     // UP主ID (用于投稿)
    pub season_id: Option<String>, // 番剧season_id
    pub media_id: Option<String>,  // 番剧media_id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_seasons: Option<Vec<String>>,
    // 新的双列表模式关键词过滤
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blacklist_keywords: Option<Vec<String>>, // 黑名单关键词列表
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whitelist_keywords: Option<Vec<String>>, // 白名单关键词列表
    pub case_sensitive: bool, // 是否区分大小写
    // 向后兼容的旧字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword_filters: Option<Vec<String>>, // 【已废弃】关键词过滤器列表
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword_filter_mode: Option<String>, // 【已废弃】关键词过滤模式
    // 音频和下载选项
    pub audio_only: bool,                  // 是否仅下载音频（输出m4a）
    pub audio_only_m4a_only: bool,         // 仅音频时只保留m4a（不下载封面/nfo/弹幕/字幕）
    pub flat_folder: bool,                 // 是否启用平铺目录模式
    pub download_danmaku: bool,            // 是否下载弹幕文件
    pub download_subtitle: bool,           // 是否下载字幕文件
    pub ai_rename: bool,                   // 是否启用AI重命名
    pub ai_rename_video_prompt: String,    // AI重命名视频提示词
    pub ai_rename_audio_prompt: String,    // AI重命名音频提示词
    pub ai_rename_enable_multi_page: bool, // 对多P视频启用AI重命名
    pub ai_rename_enable_collection: bool, // 对合集视频启用AI重命名
    pub ai_rename_enable_bangumi: bool,    // 对番剧启用AI重命名
    pub ai_rename_rename_parent_dir: bool, // AI重命名时重命名上级目录
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_dynamic_api: Option<bool>, // 投稿源：是否使用动态API
}

#[derive(Serialize, ToSchema)]
pub struct PageInfo {
    pub id: i32,
    pub pid: i32,
    pub name: String,
    pub download_status: [u32; 5],
    pub path: Option<String>,
}

impl From<(i32, i32, String, u32)> for PageInfo {
    fn from((id, pid, name, download_status): (i32, i32, String, u32)) -> Self {
        Self {
            id,
            pid,
            name,
            download_status: PageStatus::from(download_status).into(),
            path: None,
        }
    }
}

impl From<(i32, i32, String, u32, Option<String>)> for PageInfo {
    fn from((id, pid, name, download_status, path): (i32, i32, String, u32, Option<String>)) -> Self {
        Self {
            id,
            pid,
            name,
            download_status: PageStatus::from(download_status).into(),
            path,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct VideoInfo {
    pub id: i32,
    pub bvid: String,
    pub name: String,
    pub upper_name: String,
    pub path: String,
    pub category: i32,
    pub download_status: [u32; 5],
    pub cover: String,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bangumi_title: Option<String>, // 番剧真实标题，用于番剧类型视频的显示
}

impl From<(i32, String, String, String, String, i32, u32, String, bool)> for VideoInfo {
    fn from(
        (id, bvid, name, upper_name, path, category, download_status, cover, valid): (
            i32,
            String,
            String,
            String,
            String,
            i32,
            u32,
            String,
            bool,
        ),
    ) -> Self {
        Self {
            id,
            bvid,
            name,
            upper_name,
            path,
            category,
            download_status: VideoStatus::from(download_status).into(),
            cover,
            valid,
            bangumi_title: None, // 默认为None，将在API层根据视频类型填充
        }
    }
}

impl From<(i32, String, String, String, String, i32, u32, String)> for VideoInfo {
    fn from(
        (id, bvid, name, upper_name, path, category, download_status, cover): (
            i32,
            String,
            String,
            String,
            String,
            i32,
            u32,
            String,
        ),
    ) -> Self {
        Self::from((id, bvid, name, upper_name, path, category, download_status, cover, true))
    }
}

// 获取配置的响应结构体
#[derive(Serialize, ToSchema)]
pub struct ConfigResponse {
    pub video_name: String,
    pub page_name: String,
    pub multi_page_name: String,
    pub bangumi_name: String,
    pub folder_structure: String,
    pub bangumi_folder_name: String,
    pub collection_folder_mode: String,
    pub collection_unified_name: String,
    pub time_format: String,
    pub interval: u64,
    pub nfo_time_type: String,
    // 多线程下载配置
    pub parallel_download_enabled: bool,
    pub parallel_download_threads: usize,
    pub parallel_download_use_aria2: bool,
    // 视频质量设置
    pub video_max_quality: String,
    pub video_min_quality: String,
    pub audio_max_quality: String,
    pub audio_min_quality: String,
    pub codecs: Vec<String>,
    pub no_dolby_video: bool,
    pub no_dolby_audio: bool,
    pub no_hdr: bool,
    pub no_hires: bool,
    // 弹幕设置
    pub danmaku_duration: f64,
    pub danmaku_font: String,
    pub danmaku_font_size: u32,
    pub danmaku_width_ratio: f64,
    pub danmaku_horizontal_gap: f64,
    pub danmaku_lane_size: u32,
    pub danmaku_float_percentage: f64,
    pub danmaku_bottom_percentage: f64,
    pub danmaku_opacity: u8,
    pub danmaku_bold: bool,
    pub danmaku_outline: f64,
    pub danmaku_time_offset: f64,
    // 并发控制设置
    pub concurrent_video: usize,
    pub concurrent_page: usize,
    pub rate_limit: Option<usize>,
    pub rate_duration: Option<u64>,
    // 其他设置
    pub cdn_sorting: bool,
    // UP主投稿风控配置
    pub large_submission_threshold: usize,
    pub base_request_delay: u64,
    pub large_submission_delay_multiplier: u64,
    pub enable_progressive_delay: bool,
    pub max_delay_multiplier: u64,
    pub enable_incremental_fetch: bool,
    pub incremental_fallback_to_full: bool,
    pub enable_batch_processing: bool,
    pub batch_size: usize,
    pub batch_delay_seconds: u64,
    pub enable_auto_backoff: bool,
    pub auto_backoff_base_seconds: u64,
    pub auto_backoff_max_multiplier: u64,
    pub source_delay_seconds: u64,
    pub submission_source_delay_seconds: u64,
    // UP主投稿源扫描策略
    pub submission_scan_batch_size: usize,
    pub submission_adaptive_scan: bool,
    pub submission_adaptive_max_hours: u64,
    // 系统设置
    pub scan_deleted_videos: bool,
    // aria2监控配置
    pub enable_aria2_health_check: bool,
    pub enable_aria2_auto_restart: bool,
    pub aria2_health_check_interval: u64,
    // 多P视频目录结构配置
    pub multi_page_use_season_structure: bool,
    // 合集目录结构配置
    pub collection_use_season_structure: bool,
    // 番剧目录结构配置
    pub bangumi_use_season_structure: bool,
    // UP主头像保存路径
    pub upper_path: String,
    // ffmpeg 路径（可填 ffmpeg.exe 文件路径或其所在目录）
    pub ffmpeg_path: String,
    // B站凭证信息
    pub credential: Option<CredentialInfo>,
    // 推送通知配置
    pub notification: NotificationConfigResponse,
    // 风控验证配置
    pub risk_control: RiskControlConfigResponse,
    // AI重命名配置
    pub ai_rename: AiRenameConfigResponse,
    // 服务器绑定地址
    pub bind_address: String,
}

// B站凭证信息结构体
#[derive(Serialize, ToSchema)]
pub struct CredentialInfo {
    pub sessdata: String,
    pub bili_jct: String,
    pub buvid3: String,
    pub dedeuserid: String,
    pub ac_time_value: String,
    pub buvid4: Option<String>,
    pub dedeuserid_ckmd5: Option<String>,
}

// 更新配置的响应结构体
#[derive(Serialize, ToSchema)]
pub struct UpdateConfigResponse {
    pub success: bool,
    pub message: String,
    pub updated_files: Option<u32>,               // 重命名的文件数量
    pub resetted_nfo_videos_count: Option<usize>, // 重置的视频NFO任务数量
    pub resetted_nfo_pages_count: Option<usize>,  // 重置的页面NFO任务数量
}

// 配置管理相关响应结构体

// 配置项响应
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigItemResponse {
    pub key: String,
    pub value: serde_json::Value,
    pub updated_at: String,
}

// 配置重载响应
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigReloadResponse {
    pub success: bool,
    pub message: String,
    pub reloaded_at: String,
}

// 配置变更历史响应
#[derive(Serialize, ToSchema)]
pub struct ConfigHistoryResponse {
    pub changes: Vec<ConfigChangeInfo>,
    pub total: usize,
}

#[derive(Serialize, ToSchema)]
pub struct ConfigChangeInfo {
    pub id: i32,
    pub key_name: String,
    pub old_value: Option<String>,
    pub new_value: String,
    pub changed_at: String,
}

// 配置验证响应
#[derive(Serialize, ToSchema)]
pub struct ConfigValidationResponse {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

// 热重载状态响应
#[derive(Serialize, ToSchema)]
pub struct HotReloadStatusResponse {
    pub enabled: bool,
    pub last_reload: Option<String>,
    pub pending_changes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BangumiSeasonInfo {
    pub season_id: String,
    pub season_title: String,
    pub full_title: Option<String>, // 完整的番剧标题
    pub media_id: Option<String>,
    pub cover: Option<String>,
    pub episode_count: Option<i32>,  // 集数
    pub description: Option<String>, // 简介
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BangumiSeasonsResponse {
    pub success: bool,
    pub data: Vec<BangumiSeasonInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SearchResult {
    pub result_type: String,       // video, bili_user, media_bangumi等
    pub title: String,             // 标题
    pub author: String,            // 作者/UP主
    pub bvid: Option<String>,      // 视频BV号
    pub aid: Option<i64>,          // 视频AV号
    pub mid: Option<i64>,          // UP主ID
    pub season_id: Option<String>, // 番剧season_id
    pub media_id: Option<String>,  // 番剧media_id
    pub cover: String,             // 封面图
    pub description: String,       // 描述
    pub duration: Option<String>,  // 视频时长
    pub pubdate: Option<i64>,      // 发布时间
    pub play: Option<i64>,         // 播放量
    pub danmaku: Option<i64>,      // 弹幕数
    pub follower: Option<i64>,     // 粉丝数（UP主搜索结果）
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SearchResponse {
    pub success: bool,
    pub results: Vec<SearchResult>,
    pub total: u32,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UserFavoriteFolder {
    /// 收藏夹完整ID（推荐使用）
    #[serde(serialize_with = "serialize_i64_as_string")]
    pub id: i64,
    /// 收藏夹短ID（可能截断，不推荐直接使用）
    #[serde(serialize_with = "serialize_i64_as_string")]
    pub fid: i64,
    /// 收藏夹标题
    pub title: String,
    /// 收藏夹内视频数量
    pub media_count: i32,
}

// 辅助函数：将 i64 序列化为字符串
fn serialize_i64_as_string<S>(value: &i64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UserCollection {
    /// 合集类型：season（合集）或 series（系列）
    pub collection_type: String,
    /// 合集/系列ID
    pub sid: String,
    /// 合集/系列名称
    pub name: String,
    /// 封面图片URL
    pub cover: String,
    /// 描述
    pub description: String,
    /// 视频总数
    pub total: i64,
    /// 发布时间
    pub ptime: Option<i64>,
    /// UP主ID
    pub mid: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UserCollectionInfo {
    /// 合集/系列ID
    pub sid: String,
    /// 合集/系列名称
    pub name: String,
    /// 封面图片URL
    pub cover: String,
    /// 描述
    pub description: String,
    /// 视频总数
    pub total: i32,
    /// 合集类型：season（合集）或 series（系列）
    pub collection_type: String,
    /// UP主名称
    pub up_name: String,
    /// UP主ID
    pub up_mid: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UserCollectionsResponse {
    pub success: bool,
    pub collections: Vec<UserCollection>,
    pub total: u32,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Serialize, ToSchema)]
pub struct UserFollowing {
    pub mid: i64,
    pub name: String,
    pub face: String,
    pub sign: String,
    pub official_verify: Option<OfficialVerify>,
    pub follower: Option<i64>, // 粉丝数
}

#[derive(Serialize, ToSchema)]
pub struct OfficialVerify {
    #[serde(rename = "type")]
    pub type_: i32,
    pub desc: String,
}

// 初始设置相关响应

// 初始设置检查响应
#[derive(Serialize, ToSchema)]
pub struct InitialSetupCheckResponse {
    pub needs_setup: bool,
    pub has_auth_token: bool,
    pub has_credential: bool,
}

// 设置API Token响应
#[derive(Serialize, ToSchema)]
pub struct SetupAuthTokenResponse {
    pub success: bool,
    pub message: String,
}

// 更新凭证响应
#[derive(Serialize, ToSchema)]
pub struct UpdateCredentialResponse {
    pub success: bool,
    pub message: String,
}

// 扫码登录相关响应

// 生成二维码响应
#[derive(Serialize, ToSchema)]
pub struct QRGenerateResponse {
    pub session_id: String,
    pub qr_url: String,
    pub expires_in: u64, // 过期时间（秒）
}

// 轮询二维码状态响应
#[derive(Serialize, ToSchema)]
pub struct QRPollResponse {
    pub status: String, // "pending", "scanned", "confirmed", "expired"
    pub message: String,
    pub user_info: Option<QRUserInfo>,
}

// 扫码登录成功后的用户信息
#[derive(Serialize, ToSchema)]
pub struct QRUserInfo {
    pub user_id: String,
    pub username: String,
    pub avatar_url: String,
}

/// 任务控制响应
#[derive(Serialize, ToSchema)]
pub struct TaskControlResponse {
    pub success: bool,
    pub message: String,
    pub is_paused: bool,
}

/// 任务控制状态响应
#[derive(Serialize, ToSchema)]
pub struct TaskControlStatusResponse {
    pub is_paused: bool,
    pub is_scanning: bool,
    pub message: String,
}

/// Beta 镜像更新检查响应
#[derive(Serialize, ToSchema, Clone)]
pub struct BetaImageUpdateStatusResponse {
    pub update_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_built_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_pushed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 视频播放信息响应
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VideoPlayInfoResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub video_streams: Vec<VideoStreamInfo>,
    pub audio_streams: Vec<AudioStreamInfo>,
    pub subtitle_streams: Vec<SubtitleStreamInfo>,
    pub video_title: String,
    pub video_duration: Option<u32>,
    pub video_quality_description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_bvid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bilibili_url: Option<String>,
}

/// 视频流信息
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VideoStreamInfo {
    pub url: String,
    pub backup_urls: Vec<String>,
    pub quality: u32,
    pub quality_description: String,
    pub codecs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 音频流信息
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AudioStreamInfo {
    pub url: String,
    pub backup_urls: Vec<String>,
    pub quality: u32,
    pub quality_description: String,
}

/// 字幕信息
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubtitleStreamInfo {
    pub language: String,
    pub language_doc: String,
    pub url: String,
}

/// 验证收藏夹响应
#[derive(Serialize, ToSchema)]
pub struct ValidateFavoriteResponse {
    pub valid: bool,
    pub fid: i64,
    pub title: String,
    pub message: String,
}

/// 仪表盘响应
#[derive(Serialize, ToSchema)]
pub struct DashBoardResponse {
    pub enabled_favorites: u64,
    pub enabled_collections: u64,
    pub enabled_submissions: u64,
    pub enabled_bangumi: u64,
    pub enable_watch_later: bool,
    pub total_favorites: u64,
    pub total_collections: u64,
    pub total_submissions: u64,
    pub total_bangumi: u64,
    pub total_watch_later: u64,
    pub videos_by_day: Vec<DayCountPair>,
    /// 当前监听状态
    pub monitoring_status: MonitoringStatus,
}

/// 监听状态信息
#[derive(Serialize, ToSchema)]
pub struct MonitoringStatus {
    pub total_sources: u64,
    pub active_sources: u64,
    pub inactive_sources: u64,
    pub last_scan_time: Option<String>,
    pub next_scan_time: Option<String>,
    pub is_scanning: bool,
}

/// 每日视频计数
#[derive(Serialize, ToSchema, FromQueryResult)]
pub struct DayCountPair {
    pub day: String,
    pub cnt: i64,
}

/// 系统信息
#[derive(Serialize, ToSchema)]
pub struct SysInfo {
    pub total_memory: u64,
    pub used_memory: u64,
    pub process_memory: u64,
    pub used_cpu: f32,
    pub process_cpu: f32,
    pub total_disk: u64,
    pub available_disk: u64,
}

// 推送配置响应
#[derive(Serialize, ToSchema)]
pub struct NotificationConfigResponse {
    pub active_channel: String,
    pub serverchan_key: Option<String>,
    pub serverchan3_uid: Option<String>,
    pub serverchan3_sendkey: Option<String>,
    pub wecom_webhook_url: Option<String>,
    pub wecom_msgtype: String,
    pub wecom_mention_all: bool,
    pub wecom_mentioned_list: Option<Vec<String>>,
    pub enable_scan_notifications: bool,
    pub notification_min_videos: usize,
    pub notification_timeout: u64,
    pub notification_retry_count: u8,
}

// 测试推送响应
#[derive(Serialize, ToSchema)]
pub struct TestNotificationResponse {
    pub success: bool,
    pub message: String,
}

// 推送状态响应
#[derive(Serialize, ToSchema)]
pub struct NotificationStatusResponse {
    pub configured: bool,
    pub enabled: bool,
    pub last_notification_time: Option<String>,
}

// 风控验证配置响应
#[derive(Serialize, ToSchema)]
pub struct RiskControlConfigResponse {
    pub enabled: bool,
    pub mode: String,
    pub timeout: u64,
    // 自动验证配置
    pub auto_solve: Option<AutoSolveConfigResponse>,
}

// 自动验证配置响应
#[derive(Serialize, ToSchema)]
pub struct AutoSolveConfigResponse {
    pub service: String,
    pub api_key: String,
    pub max_retries: u32,
    pub solve_timeout: u64,
}

// AI重命名配置响应
#[derive(Serialize, ToSchema)]
pub struct AiRenameConfigResponse {
    pub enabled: bool,
    pub provider: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub deepseek_web_token: Option<String>,
    pub model: String,
    pub timeout_seconds: u64,
    pub video_prompt_hint: String,
    pub audio_prompt_hint: String,
    pub rename_parent_dir: bool,
}

// 测试风控验证响应
#[derive(Serialize, ToSchema)]
pub struct TestRiskControlResponse {
    pub success: bool,
    pub message: String,
    pub verification_url: Option<String>,
    pub instructions: Option<String>,
}

// 更新关键词过滤器响应（支持双列表模式）
#[derive(Serialize, ToSchema)]
pub struct UpdateKeywordFiltersResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub blacklist_count: usize, // 黑名单关键词数量
    pub whitelist_count: usize, // 白名单关键词数量
    pub message: String,
}

// 获取关键词过滤器响应（支持双列表模式）
#[derive(Serialize, ToSchema)]
pub struct GetKeywordFiltersResponse {
    pub success: bool,
    pub source_id: i32,
    pub source_type: String,
    pub blacklist_keywords: Vec<String>, // 黑名单关键词列表
    pub whitelist_keywords: Vec<String>, // 白名单关键词列表
    pub case_sensitive: bool,            // 是否区分大小写
    // 向后兼容的旧字段
    pub keyword_filters: Vec<String>,        // 【已废弃】关键词过滤器列表
    pub keyword_filter_mode: Option<String>, // 【已废弃】关键词过滤模式
}

// 验证正则表达式响应
#[derive(Serialize, ToSchema)]
pub struct ValidateRegexResponse {
    pub valid: bool,
    pub pattern: String,
    pub error: Option<String>,
}

// 清除AI缓存响应
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct ClearAiCacheResponse {
    pub success: bool,
    pub message: String,
}

// 最新入库项响应
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct LatestIngestItemResponse {
    pub video_id: i32,
    pub video_name: String,
    pub upper_name: String,
    pub path: String,
    /// 入库/完成时间（北京时间，标准格式）
    pub ingested_at: String,
    /// 平均下载速度（Bytes/s），仅统计媒体流下载阶段
    pub download_speed_bps: Option<u64>,
    /// 状态：success, failed, deleted
    pub status: String,
    /// 番剧系列名称（从share_copy的《》中提取）
    pub series_name: Option<String>,
}

// 最新入库列表响应
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct LatestIngestResponse {
    pub items: Vec<LatestIngestItemResponse>,
}

// AI批量重命名请求
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default)]
pub struct BatchRenameRequest {
    /// 视频重命名提示词（可选，为空则使用视频源配置或全局配置）
    #[serde(default)]
    pub video_prompt: String,
    /// 音频重命名提示词（可选，为空则使用视频源配置或全局配置）
    #[serde(default)]
    pub audio_prompt: String,
    /// 对多P视频启用AI重命名（可选，为None则使用全局配置）
    #[serde(default)]
    pub enable_multi_page: Option<bool>,
    /// 对合集视频启用AI重命名（可选，为None则使用全局配置）
    #[serde(default)]
    pub enable_collection: Option<bool>,
    /// 对番剧启用AI重命名（可选，为None则使用全局配置）
    #[serde(default)]
    pub enable_bangumi: Option<bool>,
    /// AI重命名时是否重命名上级目录（可选，为None则使用全局配置）
    #[serde(default)]
    pub rename_parent_dir: Option<bool>,
}

// AI批量重命名响应
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct BatchRenameResponse {
    pub success: bool,
    pub renamed_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct ConfigMigrationStatusResponse {
    pub current_version: i32,
    pub latest_version: i32,
    pub pending: bool,
    pub legacy_detected: bool,
    pub last_migrated_at: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ConfigMigrationReportResponse {
    pub current_version: i32,
    pub target_version: i32,
    pub applied: bool,
    pub dry_run: bool,
    pub legacy_detected: bool,
    pub mapped_keys: Vec<String>,
    pub unmapped_keys: Vec<String>,
    pub notes: Vec<String>,
}
