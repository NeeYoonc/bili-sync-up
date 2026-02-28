// API 响应包装器

export interface ApiResponse<T> {
	status_code: number;
	data: T;
}

// 排序字段枚举
export type SortBy = 'id' | 'name' | 'upper_name' | 'created_at' | 'pubtime';

// 排序顺序枚举
export type SortOrder = 'asc' | 'desc';

// 请求参数类型
export interface VideosRequest {
	collection?: number;
	favorite?: number;
	submission?: number;
	watch_later?: number;
	bangumi?: number;
	query?: string;
	page?: number;
	page_size?: number;
	show_failed_only?: boolean;
	min_height?: number;
	max_height?: number;
	sort_by?: SortBy;
	sort_order?: SortOrder;
}

// 关键词过滤模式类型
export type KeywordFilterMode = 'blacklist' | 'whitelist';

// 视频来源类型
export interface VideoSource {
	id: number;
	name: string;
	enabled: boolean;
	path: string;
	scan_deleted_videos: boolean;
	// 类型特有的ID字段
	f_id?: number; // 收藏夹ID
	s_id?: number; // 合集ID
	m_id?: number; // UP主ID (用于合集)
	collection_type?: string; // 合集类型: season/series
	upper_id?: number; // UP主ID (用于投稿)
	season_id?: string; // 番剧season_id
	media_id?: string; // 番剧media_id
	selected_seasons?: string[];
	selected_videos?: string | null; // 投稿源：选中视频（JSON字符串）
	use_dynamic_api?: boolean; // 投稿源：是否使用动态API
	// 新的双列表模式关键词过滤
	blacklist_keywords?: string[]; // 黑名单关键词列表（匹配的视频将被排除）
	whitelist_keywords?: string[]; // 白名单关键词列表（只下载匹配的视频）
	case_sensitive: boolean; // 是否区分大小写
	// 向后兼容的旧字段
	keyword_filters?: string[]; // 【已废弃】关键词过滤器列表（支持正则表达式）
	keyword_filter_mode?: KeywordFilterMode; // 【已废弃】关键词过滤模式
	// 下载选项
	audio_only: boolean; // 仅下载音频（输出m4a格式）
	audio_only_m4a_only: boolean; // 仅音频时只保留m4a（不下载封面/nfo/弹幕/字幕）
	flat_folder: boolean; // 平铺目录模式（不为每个视频创建子文件夹）
	download_danmaku: boolean; // 是否下载弹幕
	download_subtitle: boolean; // 是否下载字幕
	ai_rename: boolean; // 是否启用AI重命名
	ai_rename_video_prompt: string; // AI重命名视频提示词
	ai_rename_audio_prompt: string; // AI重命名音频提示词
	ai_rename_enable_multi_page: boolean; // 对多P视频启用AI重命名
	ai_rename_enable_collection: boolean; // 对合集视频启用AI重命名
	ai_rename_enable_bangumi: boolean; // 对番剧启用AI重命名
	ai_rename_rename_parent_dir: boolean; // AI重命名时重命名上级目录
}

// 视频来源响应类型
export interface VideoSourcesResponse {
	collection: VideoSource[];
	favorite: VideoSource[];
	submission: VideoSource[];
	watch_later: VideoSource[];
	bangumi: VideoSource[];
}

// 视频信息类型
export interface VideoInfo {
	id: number;
	bvid: string;
	name: string;
	upper_name: string;
	path: string;
	category: number;
	download_status: [number, number, number, number, number];
	cover: string;
	valid: boolean;
	bangumi_title?: string; // 番剧真实标题，用于番剧类型视频的显示
}

// 视频列表响应类型
export interface VideosResponse {
	videos: VideoInfo[];
	total_count: number;
}

// 分页信息类型
export interface PageInfo {
	id: number;
	pid: number;
	name: string;
	download_status: [number, number, number, number, number];
	path?: string;
}

// 单个视频响应类型
export interface VideoResponse {
	video: VideoInfo;
	pages: PageInfo[];
}

// 重置视频响应类型
export interface ResetVideoResponse {
	resetted: boolean;
	video: number;
	pages: number[];
}

// 批量重置所有视频响应类型
export interface ResetAllVideosResponse {
	resetted: boolean;
	resetted_videos_count: number;
	resetted_pages_count: number;
}

// 错误类型枚举
export enum ErrorType {
	Network = 'Network',
	Permission = 'Permission',
	Authentication = 'Authentication',
	Authorization = 'Authorization',
	NotFound = 'NotFound',
	RateLimit = 'RateLimit',
	ServerError = 'ServerError',
	ClientError = 'ClientError',
	Parse = 'Parse',
	Timeout = 'Timeout',
	FileSystem = 'FileSystem',
	Configuration = 'Configuration',
	RiskControl = 'RiskControl',
	Unknown = 'Unknown'
}

// 错误类型的中文描述
export const ErrorTypeMessages: Record<ErrorType, string> = {
	[ErrorType.Network]: '网络连接错误',
	[ErrorType.Permission]: '权限不足',
	[ErrorType.Authentication]: '认证失败',
	[ErrorType.Authorization]: '授权失败',
	[ErrorType.NotFound]: '资源未找到',
	[ErrorType.RateLimit]: '请求过于频繁',
	[ErrorType.ServerError]: '服务器内部错误',
	[ErrorType.ClientError]: '客户端错误',
	[ErrorType.Parse]: '解析错误',
	[ErrorType.Timeout]: '超时错误',
	[ErrorType.FileSystem]: '文件系统错误',
	[ErrorType.Configuration]: '配置错误',
	[ErrorType.RiskControl]: '风控触发',
	[ErrorType.Unknown]: '未知错误'
};

// 分类后的错误信息
export interface ClassifiedError {
	error_type: ErrorType;
	message: string;
	status_code?: number;
	should_retry: boolean;
	should_ignore: boolean;
	user_friendly_message?: string;
}

// API 错误类型
export interface ApiError extends ClassifiedError {
	status?: number;
	timestamp?: string;
	request_id?: string;
}

// 添加视频源请求类型
export interface AddVideoSourceRequest {
	source_type: string;
	source_id: string;
	up_id?: string;
	name: string;
	path: string;
	cover?: string;
	collection_type?: string;
	media_id?: string;
	ep_id?: string;
	download_all_seasons?: boolean;
	selected_seasons?: string[];
	selected_videos?: string[];
	merge_to_source_id?: number;
	keyword_filters?: string[]; // 关键词过滤器列表（支持正则表达式）
	keyword_filter_mode?: KeywordFilterMode; // 关键词过滤模式: "blacklist"（排除匹配）或 "whitelist"（只下载匹配）
	// 下载选项
	audio_only?: boolean; // 仅下载音频（输出m4a格式）
	audio_only_m4a_only?: boolean; // 仅音频时只保留m4a（不下载封面/nfo/弹幕/字幕）
	flat_folder?: boolean; // 平铺目录模式（不为每个视频创建子文件夹）
	download_danmaku?: boolean; // 是否下载弹幕（默认true）
	download_subtitle?: boolean; // 是否下载字幕（默认true）
	use_dynamic_api?: boolean; // 投稿源：是否使用动态API
	ai_rename?: boolean; // 是否启用AI重命名（默认false）
	ai_rename_video_prompt?: string; // AI重命名视频提示词
	ai_rename_audio_prompt?: string; // AI重命名音频提示词
	// AI重命名高级选项
	ai_rename_enable_multi_page?: boolean; // 对多P视频启用AI重命名
	ai_rename_enable_collection?: boolean; // 对合集视频启用AI重命名
	ai_rename_enable_bangumi?: boolean; // 对番剧启用AI重命名
	ai_rename_rename_parent_dir?: boolean; // AI重命名时重命名上级目录
}

// 添加视频源响应类型
export interface AddVideoSourceResponse {
	success: boolean;
	message: string;
	source_id?: number;
}

// 删除视频源响应类型
export interface DeleteVideoSourceResponse {
	success: boolean;
	message: string;
}

// 删除视频响应类型
export interface DeleteVideoResponse {
	success: boolean;
	video_id: number;
	message: string;
}

// 配置响应类型
export interface ConfigResponse {
	video_name: string;
	page_name: string;
	multi_page_name?: string;
	bangumi_name?: string;
	folder_structure: string;
	bangumi_folder_name?: string;
	collection_folder_mode?: string;
	collection_unified_name?: string;
	time_format: string;
	interval: number;
	nfo_time_type: string;
	parallel_download_enabled: boolean;
	parallel_download_threads: number;
	parallel_download_use_aria2: boolean;
	// 新增视频质量设置
	video_max_quality?: string;
	video_min_quality?: string;
	audio_max_quality?: string;
	audio_min_quality?: string;
	codecs?: string[];
	no_dolby_video?: boolean;
	no_dolby_audio?: boolean;
	no_hdr?: boolean;
	no_hires?: boolean;
	// 新增弹幕设置
	danmaku_duration?: number;
	danmaku_font?: string;
	danmaku_font_size?: number;
	danmaku_width_ratio?: number;
	danmaku_horizontal_gap?: number;
	danmaku_lane_size?: number;
	danmaku_float_percentage?: number;
	danmaku_bottom_percentage?: number;
	danmaku_opacity?: number;
	danmaku_bold?: boolean;
	danmaku_outline?: number;
	danmaku_time_offset?: number;
	// 新增并发控制设置
	concurrent_video?: number;
	concurrent_page?: number;
	rate_limit?: number;
	rate_duration?: number;
	// 新增其他设置
	cdn_sorting?: boolean;
	// UP主投稿风控配置
	large_submission_threshold?: number;
	base_request_delay?: number;
	large_submission_delay_multiplier?: number;
	enable_progressive_delay?: boolean;
	max_delay_multiplier?: number;
	enable_incremental_fetch?: boolean;
	incremental_fallback_to_full?: boolean;
	enable_batch_processing?: boolean;
	batch_size?: number;
	batch_delay_seconds?: number;
	enable_auto_backoff?: boolean;
	auto_backoff_base_seconds?: number;
	auto_backoff_max_multiplier?: number;
	source_delay_seconds?: number;
	submission_source_delay_seconds?: number;
	// UP主投稿源扫描策略
	submission_scan_batch_size?: number;
	submission_adaptive_scan?: boolean;
	submission_adaptive_max_hours?: number;
	// 扫描已删除视频设置
	scan_deleted_videos?: boolean;
	// aria2监控配置
	enable_aria2_health_check?: boolean;
	enable_aria2_auto_restart?: boolean;
	aria2_health_check_interval?: number;
	// 多P视频目录结构配置
	multi_page_use_season_structure?: boolean;
	// 合集目录结构配置
	collection_use_season_structure?: boolean;
	// 番剧目录结构配置
	bangumi_use_season_structure?: boolean;
	// B站凭证信息
	credential?: {
		sessdata: string;
		bili_jct: string;
		buvid3?: string;
		buvid4?: string;
		dedeuserid: string;
		dedeuserid_ckmd5?: string;
		ac_time_value: string;
	};
	// UP主头像保存路径
	upper_path?: string;
	// ffmpeg 路径（可填 ffmpeg.exe 文件路径或其所在目录）
	ffmpeg_path?: string;
	// 风控验证配置
	risk_control?: {
		enabled: boolean;
		mode: string;
		timeout: number;
		auto_solve?: {
			service?: string;
			api_key?: string;
			max_retries?: number;
			solve_timeout?: number;
		};
	};
	// AI重命名配置
	ai_rename?: AiRenameConfig;
	// 服务器绑定地址
	bind_address: string;
}

// AI重命名配置类型
export interface AiRenameConfig {
	enabled: boolean;
	provider: string;
	base_url: string;
	api_key?: string;
	deepseek_web_token?: string;
	model: string;
	timeout_seconds: number;
	video_prompt_hint: string;
	audio_prompt_hint: string;
	rename_parent_dir: boolean;
}

// 更新配置请求类型
export interface UpdateConfigRequest {
	video_name?: string;
	page_name?: string;
	multi_page_name?: string;
	bangumi_name?: string;
	folder_structure?: string;
	bangumi_folder_name?: string;
	collection_folder_mode?: string;
	collection_unified_name?: string;
	time_format?: string;
	interval?: number;
	nfo_time_type?: string;
	parallel_download_enabled?: boolean;
	parallel_download_threads?: number;
	parallel_download_use_aria2?: boolean;
	// 新增视频质量设置
	video_max_quality?: string;
	video_min_quality?: string;
	audio_max_quality?: string;
	audio_min_quality?: string;
	codecs?: string[];
	no_dolby_video?: boolean;
	no_dolby_audio?: boolean;
	no_hdr?: boolean;
	no_hires?: boolean;
	// 新增弹幕设置
	danmaku_duration?: number;
	danmaku_font?: string;
	danmaku_font_size?: number;
	danmaku_width_ratio?: number;
	danmaku_horizontal_gap?: number;
	danmaku_lane_size?: number;
	danmaku_float_percentage?: number;
	danmaku_bottom_percentage?: number;
	danmaku_opacity?: number;
	danmaku_bold?: boolean;
	danmaku_outline?: number;
	danmaku_time_offset?: number;
	// 新增并发控制设置
	concurrent_video?: number;
	concurrent_page?: number;
	rate_limit?: number;
	rate_duration?: number;
	// 新增其他设置
	cdn_sorting?: boolean;
	// UP主投稿风控配置
	large_submission_threshold?: number;
	base_request_delay?: number;
	large_submission_delay_multiplier?: number;
	enable_progressive_delay?: boolean;
	max_delay_multiplier?: number;
	enable_incremental_fetch?: boolean;
	incremental_fallback_to_full?: boolean;
	enable_batch_processing?: boolean;
	batch_size?: number;
	batch_delay_seconds?: number;
	enable_auto_backoff?: boolean;
	auto_backoff_base_seconds?: number;
	auto_backoff_max_multiplier?: number;
	source_delay_seconds?: number;
	submission_source_delay_seconds?: number;
	// UP主投稿源扫描策略
	submission_scan_batch_size?: number;
	submission_adaptive_scan?: boolean;
	submission_adaptive_max_hours?: number;
	// 扫描已删除视频设置
	scan_deleted_videos?: boolean;
	// aria2监控配置
	enable_aria2_health_check?: boolean;
	enable_aria2_auto_restart?: boolean;
	aria2_health_check_interval?: number;
	// 多P视频目录结构配置
	multi_page_use_season_structure?: boolean;
	// 合集目录结构配置
	collection_use_season_structure?: boolean;
	// 番剧目录结构配置
	bangumi_use_season_structure?: boolean;
	// UP主头像保存路径
	upper_path?: string;
	// ffmpeg 路径（可填 ffmpeg.exe 文件路径或其所在目录）
	ffmpeg_path?: string;
	// 风控验证配置
	risk_control_enabled?: boolean;
	risk_control_mode?: string;
	risk_control_timeout?: number;
	// 自动验证配置
	risk_control_auto_solve_service?: string;
	risk_control_auto_solve_api_key?: string;
	risk_control_auto_solve_max_retries?: number;
	risk_control_auto_solve_timeout?: number;
	// AI重命名配置
	ai_rename_enabled?: boolean;
	ai_rename_provider?: string;
	ai_rename_base_url?: string;
	ai_rename_api_key?: string;
	ai_rename_deepseek_web_token?: string;
	ai_rename_model?: string;
	ai_rename_timeout_seconds?: number;
	ai_rename_video_prompt_hint?: string;
	ai_rename_audio_prompt_hint?: string;
	ai_rename_rename_parent_dir?: boolean;
	// 服务器绑定地址
	bind_address?: string;
}

// 更新配置响应类型
export interface UpdateConfigResponse {
	success: boolean;
	message: string;
	updated_files?: number;
	resetted_nfo_videos_count?: number;
	resetted_nfo_pages_count?: number;
}

// 搜索请求类型
export interface SearchRequest {
	keyword: string;
	search_type: 'video' | 'bili_user' | 'media_bangumi';
	page?: number;
	page_size?: number;
}

// 搜索结果项类型
export interface SearchResultItem {
	result_type: string;
	title: string;
	author: string;
	bvid?: string;
	aid?: number;
	mid?: number;
	season_id?: string;
	media_id?: string;
	cover: string;
	description: string;
	duration?: string;
	pubdate?: number;
	play?: number;
	danmaku?: number;
	follower?: number; // 粉丝数（UP主搜索结果）
}

// 搜索响应类型
export interface SearchResponse {
	success: boolean;
	results: SearchResultItem[];
	total: number;
	page: number;
	page_size: number;
}

// 用户信息类型
export interface UserInfo {
	user_id: string | number;
	username: string;
	avatar_url?: string;
}

// 用户收藏夹类型
export interface UserFavoriteFolder {
	id: number | string;
	fid?: number | string;
	name?: string;
	title?: string;
	media_count: number;
	cover?: string;
	created?: number;
}

// 用户合集/系列项类型
export interface UserCollectionItem {
	collection_type: string;
	sid: string;
	name: string;
	cover: string;
	description: string;
	total: number;
	ptime?: number;
	mid: number;
}

// 用户合集响应类型
export interface UserCollectionsResponse {
	success: boolean;
	collections: UserCollectionItem[];
	total: number;
	page: number;
	page_size: number;
}

// 视频分类类型
export type VideoCategory = 'collection' | 'favorite' | 'submission' | 'watch_later' | 'bangumi';

// 番剧季度信息类型
export interface BangumiSeasonInfo {
	season_id: string;
	season_title: string;
	full_title?: string;
	media_id?: string;
	cover?: string;
	episode_count?: number; // 集数
	description?: string; // 简介
}

// 番剧季度响应类型
export interface BangumiSeasonsResponse {
	success: boolean;
	data: BangumiSeasonInfo[];
}

// 番剧源选项（用于合并选择）
export interface BangumiSourceOption {
	id: number;
	name: string;
	path: string;
	season_id: string | null;
	media_id: string | null;
	download_all_seasons: boolean;
	selected_seasons_count: number;
}

// 番剧源列表响应
export interface BangumiSourceListResponse {
	success: boolean;
	bangumi_sources: BangumiSourceOption[];
	total_count: number;
}

// 关注的UP主信息类型
export interface UserFollowing {
	mid: number;
	name: string;
	face: string;
	sign: string;
	official_verify?: OfficialVerify;
	follower?: number; // 粉丝数
}

// 官方认证信息类型
export interface OfficialVerify {
	type: number;
	desc: string;
}

export interface UserCollectionInfo {
	sid: string;
	name: string;
	cover: string;
	description: string;
	total: number;
	collection_type: string;
	up_name: string;
	up_mid: number;
}

// 队列任务信息类型
export interface QueueTaskInfo {
	task_id: string;
	task_type: string;
	description: string;
	created_at: string;
}

// 队列信息类型
export interface QueueInfo {
	length: number;
	is_processing: boolean;
	tasks: QueueTaskInfo[];
}

// 配置队列信息类型
export interface ConfigQueueInfo {
	update_length: number;
	reload_length: number;
	is_processing: boolean;
	update_tasks: QueueTaskInfo[];
	reload_tasks: QueueTaskInfo[];
}

// 队列状态响应类型
export interface QueueStatusResponse {
	is_scanning: boolean;
	delete_queue: QueueInfo;
	video_delete_queue: QueueInfo;
	add_queue: QueueInfo;
	config_queue: ConfigQueueInfo;
}

export interface CancelQueueTaskResponse {
	success: boolean;
	task_id: string;
	message: string;
}

// 状态更新相关类型
export interface StatusUpdate {
	status_index: number;
	status_value: number;
}

export interface PageStatusUpdate {
	page_id: number;
	updates: StatusUpdate[];
}

export interface UpdateVideoStatusRequest {
	video_updates?: StatusUpdate[];
	page_updates?: PageStatusUpdate[];
}

export interface UpdateVideoStatusResponse {
	success: boolean;
	video: VideoInfo;
	pages: PageInfo[];
}

// 更新视频源启用状态请求类型
export interface UpdateVideoSourceEnabledRequest {
	enabled: boolean;
}

// 更新视频源启用状态响应类型
export interface UpdateVideoSourceEnabledResponse {
	success: boolean;
	source_id: number;
	source_type: string;
	enabled: boolean;
	message: string;
}

// 更新视频源扫描已删除视频设置请求类型
export interface UpdateVideoSourceScanDeletedRequest {
	scan_deleted_videos: boolean;
}

// 更新视频源扫描已删除视频设置响应类型
export interface UpdateVideoSourceScanDeletedResponse {
	success: boolean;
	source_id: number;
	source_type: string;
	scan_deleted_videos: boolean;
	message: string;
}

// 重设视频源路径请求类型
export interface ResetVideoSourcePathRequest {
	new_path: string;
	apply_rename_rules?: boolean;
	clean_empty_folders?: boolean;
}

// 重设视频源路径响应类型
export interface ResetVideoSourcePathResponse {
	success: boolean;
	source_id: number;
	source_type: string;
	old_path: string;
	new_path: string;
	moved_files_count: number;
	updated_videos_count: number;
	cleaned_folders_count: number;
	message: string;
}

// 更新投稿源选中视频列表响应类型
export interface UpdateSubmissionSelectedVideosResponse {
	success: boolean;
	source_id: number;
	selected_count: number;
	message: string;
}

// 更新关键词过滤器请求类型（支持双列表模式）
export interface UpdateKeywordFiltersRequest {
	blacklist_keywords?: string[]; // 黑名单关键词列表（匹配的视频将被排除）
	whitelist_keywords?: string[]; // 白名单关键词列表（只下载匹配的视频）
	case_sensitive?: boolean; // 是否区分大小写
	// 向后兼容的旧字段
	keyword_filters?: string[]; // 【已废弃】关键词过滤器列表
	keyword_filter_mode?: KeywordFilterMode; // 【已废弃】关键词过滤模式
}

// 更新关键词过滤器响应类型（支持双列表模式）
export interface UpdateKeywordFiltersResponse {
	success: boolean;
	source_id: number;
	source_type: string;
	blacklist_count: number; // 黑名单关键词数量
	whitelist_count: number; // 白名单关键词数量
	message: string;
}

// 获取关键词过滤器响应类型（支持双列表模式）
export interface GetKeywordFiltersResponse {
	success: boolean;
	source_id: number;
	source_type: string;
	blacklist_keywords: string[]; // 黑名单关键词列表
	whitelist_keywords: string[]; // 白名单关键词列表
	case_sensitive: boolean; // 是否区分大小写
	// 向后兼容的旧字段
	keyword_filters: string[]; // 【已废弃】关键词过滤器列表
	keyword_filter_mode?: string; // 【已废弃】关键词过滤模式
}

// 验证正则表达式请求类型
export interface ValidateRegexRequest {
	pattern: string;
}

// 验证正则表达式响应类型
export interface ValidateRegexResponse {
	valid: boolean;
	pattern: string;
	error?: string;
}

// 更新凭证请求类型
export interface UpdateCredentialRequest {
	sessdata: string;
	bili_jct: string;
	buvid3: string;
	dedeuserid: string;
	ac_time_value?: string;
}

// 更新凭证响应类型
export interface UpdateCredentialResponse {
	success: boolean;
	message: string;
}

// 初始设置检查响应类型
export interface InitialSetupCheckResponse {
	needs_setup: boolean;
	has_auth_token: boolean;
	has_credential: boolean;
}

// 任务控制响应类型
export interface TaskControlResponse {
	success: boolean;
	message: string;
	is_paused: boolean;
}

// 任务控制状态响应类型
export interface TaskControlStatusResponse {
	is_paused: boolean;
	is_scanning: boolean;
	message: string;
}

// 视频播放信息响应类型
export interface VideoPlayInfoResponse {
	success: boolean;
	message?: string;
	video_streams: VideoStreamInfo[];
	audio_streams: AudioStreamInfo[];
	subtitle_streams: SubtitleStreamInfo[];
	video_title: string;
	video_duration?: number;
	video_quality_description: string;
	video_bvid?: string;
	bilibili_url?: string;
}

// 视频BVID信息响应类型
export interface VideoBvidResponse {
	bvid: string;
	title: string;
	bilibili_url: string;
}

// 视频流信息类型
export interface VideoStreamInfo {
	url: string;
	backup_urls: string[];
	quality: number;
	quality_description: string;
	codecs: string;
	container?: string;
	width?: number;
	height?: number;
}

// 音频流信息类型
export interface AudioStreamInfo {
	url: string;
	backup_urls: string[];
	quality: number;
	quality_description: string;
}

// 字幕信息类型
export interface SubtitleStreamInfo {
	language: string;
	language_doc: string;
	url: string;
}

// 验证收藏夹响应类型
export interface ValidateFavoriteResponse {
	valid: boolean;
	fid: number;
	title: string;
	message: string;
}

// UP主投稿视频信息类型
export interface SubmissionVideoInfo {
	title: string;
	bvid: string;
	description: string;
	cover: string;
	pubtime: string;
	view: number;
	danmaku: number;
	duration: number;
}

// 获取UP主投稿列表请求类型
export interface SubmissionVideosRequest {
	up_id: string;
	page?: number;
	page_size?: number;
	keyword?: string; // 搜索关键词
}

// 获取UP主投稿列表响应类型
export interface SubmissionVideosResponse {
	videos: SubmissionVideoInfo[];
	total: number;
	page: number;
	page_size: number;
}

// 仪表盘响应类型
export interface DashBoardResponse {
	enabled_favorites: number;
	enabled_collections: number;
	enabled_submissions: number;
	enabled_bangumi: number;
	enable_watch_later: boolean;
	total_favorites: number;
	total_collections: number;
	total_submissions: number;
	total_bangumi: number;
	total_watch_later: number;
	videos_by_day: DayCountPair[];
	monitoring_status: MonitoringStatus;
}

// 监听状态类型
export interface MonitoringStatus {
	total_sources: number;
	active_sources: number;
	inactive_sources: number;
	last_scan_time: string | null;
	next_scan_time: string | null;
	is_scanning: boolean;
}

// 每日视频计数类型
export interface DayCountPair {
	day: string;
	cnt: number;
}

// 系统信息类型
export interface SysInfo {
	total_memory: number;
	used_memory: number;
	process_memory: number;
	used_cpu: number;
	process_cpu: number;
	total_disk: number;
	available_disk: number;
}

// 任务状态类型
export interface TaskStatus {
	is_running: boolean;
	last_run?: string;
	last_finish?: string;
	next_run?: string;
}

// 首页最新入库
export interface LatestIngestItem {
	video_id: number;
	video_name: string;
	upper_name: string;
	path: string;
	ingested_at: string;
	download_speed_bps: number | null;
	status: 'success' | 'failed' | 'deleted';
	series_name: string | null; // 番剧系列名称（从share_copy的《》中提取）
}

export interface LatestIngestResponse {
	items: LatestIngestItem[];
}

// beta 镜像更新检查响应
export interface BetaImageUpdateStatusResponse {
	update_available: boolean;
	release_channel?: string;
	checked_tag?: string;
	local_built_at?: string;
	remote_pushed_at?: string;
	checked_at?: string;
	error?: string;
}
