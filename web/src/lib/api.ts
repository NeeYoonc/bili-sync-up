import type {
	ApiResponse,
	VideoSourcesResponse,
	VideosRequest,
	VideosResponse,
	VideoResponse,
	ResetVideoResponse,
	ResetAllVideosResponse,
	UpdateVideoStatusRequest,
	UpdateVideoStatusResponse,
	ApiError,
	AddVideoSourceRequest,
	AddVideoSourceResponse,
	DeleteVideoSourceResponse,
	DeleteVideoResponse,
	ConfigResponse,
	UpdateConfigRequest,
	UpdateConfigResponse,
	SearchRequest,
	SearchResponse,
	UserFavoriteFolder,
	UserCollectionsResponse,
	UserFollowing,
	UserCollectionInfo,
	QueueStatusResponse,
	CancelQueueTaskResponse,
	UpdateVideoSourceEnabledResponse,
	ResetVideoSourcePathRequest,
	ResetVideoSourcePathResponse,
	UpdateSubmissionSelectedVideosResponse,
	UpdateKeywordFiltersResponse,
	GetKeywordFiltersResponse,
	ValidateRegexResponse,
	UpdateCredentialRequest,
	UpdateCredentialResponse,
	InitialSetupCheckResponse,
	TaskControlStatusResponse,
	TaskControlResponse,
	VideoPlayInfoResponse,
	ValidateFavoriteResponse,
	SubmissionVideosRequest,
	SubmissionVideosResponse,
	DashBoardResponse,
	SysInfo,
	TaskStatus,
	BangumiSeasonsResponse,
	VideoBvidResponse,
	KeywordFilterMode,
	LatestIngestResponse,
	BetaImageUpdateStatusResponse
} from './types';
import { ErrorType } from './types';
import { wsManager } from './ws';

// API 基础配置
const API_BASE_URL = '/api';

// HTTP 客户端类
class ApiClient {
	private baseURL: string;
	private defaultHeaders: Record<string, string>;

	constructor(baseURL: string = API_BASE_URL) {
		this.baseURL = baseURL;
		this.defaultHeaders = {
			'Content-Type': 'application/json'
		};
		const token = localStorage.getItem('auth_token');
		if (token) {
			this.defaultHeaders['Authorization'] = token;
		}
	}

	// 设置认证 token
	setAuthToken(token?: string) {
		if (token) {
			this.defaultHeaders['Authorization'] = token;
			localStorage.setItem('auth_token', token);
		} else {
			delete this.defaultHeaders['Authorization'];
			localStorage.removeItem('auth_token');
		}
	}

	// 通用请求方法
	private async request<T>(endpoint: string, options: RequestInit = {}): Promise<ApiResponse<T>> {
		const url = `${this.baseURL}${endpoint}`;

		const config: RequestInit = {
			headers: {
				...this.defaultHeaders,
				...options.headers
			},
			...options
		};

		try {
			const response = await fetch(url, config);

			if (!response.ok) {
				// 尝试读取响应体获取详细错误信息
				let errorMessage = `HTTP error! status: ${response.status}`;
				try {
					const errorData = await response.json();
					if (errorData && typeof errorData.data === 'string') {
						errorMessage = errorData.data;
					}
				} catch {
					// 如果无法解析JSON，使用默认错误消息
				}
				throw new Error(errorMessage);
			}

			const data: ApiResponse<T> = await response.json();
			return data;
		} catch (error) {
			const apiError: ApiError = {
				error_type: ErrorType.Unknown,
				message: error instanceof Error ? error.message : 'Unknown error occurred',
				should_retry: false,
				should_ignore: false,
				status:
					error instanceof TypeError
						? undefined
						: error &&
							  typeof error === 'object' &&
							  'status' in error &&
							  typeof error.status === 'number'
							? error.status
							: undefined,
				timestamp: new Date().toISOString()
			};
			throw apiError;
		}
	}

	// GET 请求
	private async get<T>(
		endpoint: string,
		params?: VideosRequest | Record<string, unknown>
	): Promise<ApiResponse<T>> {
		let queryString = '';

		if (params) {
			const searchParams = new URLSearchParams();
			Object.entries(params).forEach(([key, value]) => {
				if (value !== undefined && value !== null) {
					searchParams.append(key, String(value));
				}
			});
			queryString = searchParams.toString();
		}

		const finalEndpoint = queryString ? `${endpoint}?${queryString}` : endpoint;
		return this.request<T>(finalEndpoint, {
			method: 'GET'
		});
	}

	// POST 请求
	private async post<T>(endpoint: string, data?: unknown): Promise<ApiResponse<T>> {
		return this.request<T>(endpoint, {
			method: 'POST',
			body: data ? JSON.stringify(data) : undefined
		});
	}

	// PUT 请求
	private async put<T>(endpoint: string, data?: unknown): Promise<ApiResponse<T>> {
		return this.request<T>(endpoint, {
			method: 'PUT',
			body: data ? JSON.stringify(data) : undefined
		});
	}

	// DELETE 请求
	private async delete<T>(
		endpoint: string,
		params?: Record<string, string>
	): Promise<ApiResponse<T>> {
		let queryString = '';
		if (params) {
			const searchParams = new URLSearchParams(params);
			queryString = searchParams.toString();
		}
		const finalEndpoint = queryString ? `${endpoint}?${queryString}` : endpoint;
		return this.request<T>(finalEndpoint, {
			method: 'DELETE'
		});
	}

	// API 方法

	/**
	 * 获取所有视频来源
	 */
	async getVideoSources(): Promise<ApiResponse<VideoSourcesResponse>> {
		return this.get<VideoSourcesResponse>('/video-sources');
	}

	/**
	 * 获取视频列表
	 * @param params 查询参数
	 */
	async getVideos(params?: VideosRequest): Promise<ApiResponse<VideosResponse>> {
		return this.get<VideosResponse>('/videos', params);
	}

	/**
	 * 获取单个视频详情
	 * @param id 视频 ID
	 */
	async getVideo(id: number): Promise<ApiResponse<VideoResponse>> {
		return this.get<VideoResponse>(`/videos/${id}`);
	}

	/**
	 * 重置视频下载状态
	 * @param id 视频 ID
	 * @param force 是否强制重置
	 */
	async resetVideo(id: number, force: boolean = false): Promise<ApiResponse<ResetVideoResponse>> {
		const endpoint = force ? `/videos/${id}/reset?force=true` : `/videos/${id}/reset`;
		return this.post<ResetVideoResponse>(endpoint);
	}

	/**
	 * 批量重置所有视频下载状态
	 * @param params 可选的查询参数，用于筛选要重置的视频（与 /api/videos 参数保持一致的子集）
	 * @param force 是否强制重置（包括已完成的视频）
	 */
	async resetAllVideos(
		params?: VideosRequest,
		force: boolean = false
	): Promise<ApiResponse<ResetAllVideosResponse>> {
		const searchParams = new URLSearchParams();
		if (params) {
			Object.entries(params).forEach(([key, value]) => {
				if (value !== undefined) {
					searchParams.append(key, value.toString());
				}
			});
		}
		if (force) {
			searchParams.append('force', 'true');
		}
		const query = searchParams.toString();
		const endpoint = query ? `/videos/reset-all?${query}` : '/videos/reset-all';
		return this.post<ResetAllVideosResponse>(endpoint);
	}

	/**
	 * 删除视频（软删除）
	 * @param id 视频 ID
	 */
	async deleteVideo(id: number): Promise<ApiResponse<DeleteVideoResponse>> {
		return this.delete<DeleteVideoResponse>(`/videos/${id}`);
	}

	/**
	 * 选择性重置特定任务
	 * @param taskIndexes 要重置的任务索引列表
	 * @param params 可选的查询参数，用于筛选要重置的视频（与 /api/videos 参数保持一致的子集）
	 * @param force 是否强制重置（包括已完成的任务）
	 */
	async resetSpecificTasks(
		taskIndexes: number[],
		params?: VideosRequest,
		force: boolean = false
	): Promise<ApiResponse<ResetAllVideosResponse>> {
		const requestBody = {
			task_indexes: taskIndexes,
			force,
			...params
		};
		return this.post<ResetAllVideosResponse>('/videos/reset-specific-tasks', requestBody);
	}

	/**
	 * 添加视频源
	 * @param params 视频源参数
	 */
	async addVideoSource(
		params: AddVideoSourceRequest
	): Promise<ApiResponse<AddVideoSourceResponse>> {
		return this.post<AddVideoSourceResponse>('/video-sources', params);
	}

	/**
	 * 删除视频源
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param deleteLocalFiles 是否删除本地文件
	 */
	async deleteVideoSource(
		sourceType: string,
		id: number,
		deleteLocalFiles: boolean = false
	): Promise<ApiResponse<DeleteVideoSourceResponse>> {
		return this.delete<DeleteVideoSourceResponse>(`/video-sources/${sourceType}/${id}`, {
			delete_local_files: deleteLocalFiles.toString()
		});
	}

	/**
	 * 更新视频源启用状态
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param enabled 是否启用
	 */
	async updateVideoSourceEnabled(
		sourceType: string,
		id: number,
		enabled: boolean
	): Promise<ApiResponse<UpdateVideoSourceEnabledResponse>> {
		return this.put<UpdateVideoSourceEnabledResponse>(
			`/video-sources/${sourceType}/${id}/enabled`,
			{ enabled }
		);
	}

	/**
	 * 更新视频源扫描已删除视频设置
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param scanDeleted 是否扫描已删除视频
	 */
	async updateVideoSourceScanDeleted(
		sourceType: string,
		id: number,
		scanDeleted: boolean
	): Promise<ApiResponse<UpdateVideoSourceEnabledResponse>> {
		return this.put<UpdateVideoSourceEnabledResponse>(
			`/video-sources/${sourceType}/${id}/scan-deleted`,
			{ scan_deleted_videos: scanDeleted }
		);
	}

	/**
	 * 更新视频源下载选项
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param options 下载选项
	 */
	async updateVideoSourceDownloadOptions(
		sourceType: string,
		id: number,
		options: {
			audio_only?: boolean;
			audio_only_m4a_only?: boolean;
			flat_folder?: boolean;
			download_danmaku?: boolean;
			download_subtitle?: boolean;
			use_dynamic_api?: boolean;
			ai_rename?: boolean;
			ai_rename_video_prompt?: string;
			ai_rename_audio_prompt?: string;
			ai_rename_enable_multi_page?: boolean;
			ai_rename_enable_collection?: boolean;
			ai_rename_enable_bangumi?: boolean;
			ai_rename_rename_parent_dir?: boolean;
		}
	): Promise<
		ApiResponse<{
			success: boolean;
			source_id: number;
			source_type: string;
			audio_only: boolean;
			audio_only_m4a_only: boolean;
			flat_folder: boolean;
			download_danmaku: boolean;
			download_subtitle: boolean;
			ai_rename: boolean;
			ai_rename_video_prompt: string;
			ai_rename_audio_prompt: string;
			ai_rename_enable_multi_page: boolean;
			ai_rename_enable_collection: boolean;
			ai_rename_enable_bangumi: boolean;
			ai_rename_rename_parent_dir: boolean;
			use_dynamic_api: boolean;
			message: string;
		}>
	> {
		return this.put<{
			success: boolean;
			source_id: number;
			source_type: string;
			audio_only: boolean;
			audio_only_m4a_only: boolean;
			flat_folder: boolean;
			download_danmaku: boolean;
			download_subtitle: boolean;
			ai_rename: boolean;
			ai_rename_video_prompt: string;
			ai_rename_audio_prompt: string;
			ai_rename_enable_multi_page: boolean;
			ai_rename_enable_collection: boolean;
			ai_rename_enable_bangumi: boolean;
			ai_rename_rename_parent_dir: boolean;
			use_dynamic_api: boolean;
			message: string;
		}>(`/video-sources/${sourceType}/${id}/download-options`, options);
	}

	/**
	 * 重设视频源路径
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param params 路径重设参数
	 */
	async resetVideoSourcePath(
		sourceType: string,
		id: number,
		params: ResetVideoSourcePathRequest
	): Promise<ApiResponse<ResetVideoSourcePathResponse>> {
		return this.post<ResetVideoSourcePathResponse>(
			`/video-sources/${sourceType}/${id}/reset-path`,
			params
		);
	}

	/**
	 * 更新投稿源选中视频列表
	 * @param id 投稿源ID
	 * @param selectedVideos 选中的视频BVID列表
	 */
	async updateSubmissionSelectedVideos(
		id: number,
		selectedVideos: string[]
	): Promise<ApiResponse<UpdateSubmissionSelectedVideosResponse>> {
		return this.put<UpdateSubmissionSelectedVideosResponse>(
			`/video-sources/submission/${id}/selected-videos`,
			{ selected_videos: selectedVideos }
		);
	}

	/**
	 * 获取视频源关键词过滤器
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 */
	async getVideoSourceKeywordFilters(
		sourceType: string,
		id: number
	): Promise<ApiResponse<GetKeywordFiltersResponse>> {
		return this.get<GetKeywordFiltersResponse>(
			`/video-sources/${sourceType}/${id}/keyword-filters`
		);
	}

	/**
	 * 更新视频源关键词过滤器（双列表模式）
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param blacklistKeywords 黑名单关键词列表
	 * @param whitelistKeywords 白名单关键词列表
	 */
	async updateVideoSourceKeywordFilters(
		sourceType: string,
		id: number,
		blacklistKeywords: string[],
		whitelistKeywords: string[],
		caseSensitive: boolean = true
	): Promise<ApiResponse<UpdateKeywordFiltersResponse>> {
		return this.put<UpdateKeywordFiltersResponse>(
			`/video-sources/${sourceType}/${id}/keyword-filters`,
			{
				blacklist_keywords: blacklistKeywords,
				whitelist_keywords: whitelistKeywords,
				case_sensitive: caseSensitive
			}
		);
	}

	/**
	 * 验证正则表达式
	 * @param pattern 正则表达式
	 */
	async validateRegex(pattern: string): Promise<ApiResponse<ValidateRegexResponse>> {
		return this.post<ValidateRegexResponse>('/validate-regex', { pattern });
	}

	/**
	 * 清除AI对话历史缓存
	 */
	async clearAiRenameCache(): Promise<ApiResponse<{ success: boolean; message: string }>> {
		return this.post<{ success: boolean; message: string }>('/ai-rename/clear-cache', {});
	}

	/**
	 * 清除指定视频源的AI对话历史缓存
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 */
	async clearAiRenameCacheForSource(
		sourceType: string,
		id: number
	): Promise<ApiResponse<{ success: boolean; message: string }>> {
		return this.post<{ success: boolean; message: string }>(
			`/ai-rename/clear-cache/${sourceType}/${id}`,
			{}
		);
	}

	/**
	 * 批量AI重命名视频源下的历史文件
	 * @param sourceType 视频源类型 (collection/favorite/submission/watch_later)
	 * @param id 视频源ID
	 * @param videoPrompt 视频重命名提示词（可选）
	 * @param audioPrompt 音频重命名提示词（可选）
	 * @param enableMultiPage 对多P视频启用AI重命名（可选，为空则使用全局配置）
	 * @param enableCollection 对合集视频启用AI重命名（可选，为空则使用全局配置）
	 * @param enableBangumi 对番剧启用AI重命名（可选，为空则使用全局配置）
	 */
	async aiRenameHistory(
		sourceType: string,
		id: number,
		videoPrompt?: string,
		audioPrompt?: string,
		enableMultiPage?: boolean,
		enableCollection?: boolean,
		enableBangumi?: boolean,
		renameParentDir?: boolean
	): Promise<
		ApiResponse<{
			success: boolean;
			renamed_count: number;
			skipped_count: number;
			failed_count: number;
			message: string;
		}>
	> {
		return this.post<{
			success: boolean;
			renamed_count: number;
			skipped_count: number;
			failed_count: number;
			message: string;
		}>(`/${sourceType}/${id}/ai-rename-history`, {
			video_prompt: videoPrompt || '',
			audio_prompt: audioPrompt || '',
			enable_multi_page: enableMultiPage,
			enable_collection: enableCollection,
			enable_bangumi: enableBangumi,
			rename_parent_dir: renameParentDir
		});
	}

	/**
	 * 获取配置
	 */
	async getConfig(): Promise<ApiResponse<ConfigResponse>> {
		return this.get<ConfigResponse>('/config');
	}

	/**
	 * 更新配置
	 * @param params 配置参数
	 */
	async updateConfig(params: UpdateConfigRequest): Promise<ApiResponse<UpdateConfigResponse>> {
		return this.put<UpdateConfigResponse>('/config', params);
	}

	/**
	 * 搜索B站内容
	 * @param params 搜索参数
	 */
	async searchBilibili(params: SearchRequest): Promise<ApiResponse<SearchResponse>> {
		return this.get<SearchResponse>('/search', params);
	}

	/**
	 * 获取用户收藏夹列表
	 */
	async getUserFavorites(): Promise<ApiResponse<UserFavoriteFolder[]>> {
		return this.get<UserFavoriteFolder[]>('/user/favorites');
	}

	/**
	 * 验证收藏夹ID并获取收藏夹信息
	 * @param fid 收藏夹ID
	 */
	async validateFavorite(fid: string): Promise<ApiResponse<ValidateFavoriteResponse>> {
		return this.get<ValidateFavoriteResponse>(`/favorite/${fid}/validate`);
	}

	/**
	 * 获取指定UP主的收藏夹列表
	 * @param uid UP主ID
	 */
	async getUserFavoritesByUid(uid: string): Promise<ApiResponse<UserFavoriteFolder[]>> {
		return this.get<UserFavoriteFolder[]>(`/user/${uid}/favorites`);
	}

	/**
	 * 获取UP主的合集和系列列表
	 * @param mid UP主ID
	 * @param page 页码
	 * @param pageSize 每页数量
	 */
	async getUserCollections(
		mid: string,
		page: number = 1,
		pageSize: number = 20
	): Promise<ApiResponse<UserCollectionsResponse>> {
		return this.get<UserCollectionsResponse>(`/user/collections/${mid}`, {
			page,
			page_size: pageSize
		});
	}

	/**
	 * 获取番剧季度信息
	 */
	async getBangumiSeasons(seasonId: string): Promise<ApiResponse<BangumiSeasonsResponse>> {
		return this.get<BangumiSeasonsResponse>(`/bangumi/seasons/${seasonId}`);
	}

	/**
	 * 获取现有番剧源列表（用于合并选择）
	 */
	async getBangumiSourcesForMerge(): Promise<
		ApiResponse<import('./types').BangumiSourceListResponse>
	> {
		return this.get<import('./types').BangumiSourceListResponse>('/video-sources/bangumi/list');
	}

	/**
	 * 获取关注的UP主列表
	 */
	async getUserFollowings(): Promise<ApiResponse<UserFollowing[]>> {
		return this.get<UserFollowing[]>('/user/followings');
	}

	/**
	 * 获取订阅的合集列表
	 */
	async getSubscribedCollections(): Promise<ApiResponse<UserCollectionInfo[]>> {
		return this.get<UserCollectionInfo[]>('/user/subscribed-collections');
	}

	/**
	 * 获取队列状态
	 */
	async getQueueStatus(): Promise<ApiResponse<QueueStatusResponse>> {
		return this.get<QueueStatusResponse>('/queue-status');
	}

	/**
	 * 取消队列中的待处理任务
	 * @param taskId 任务ID
	 */
	async cancelQueueTask(taskId: string): Promise<ApiResponse<CancelQueueTaskResponse>> {
		return this.delete<CancelQueueTaskResponse>(`/queue/tasks/${encodeURIComponent(taskId)}`);
	}

	/**
	 * 更新视频状态
	 * @param id 视频ID
	 * @param request 状态更新请求
	 */
	async updateVideoStatus(
		id: number,
		request: UpdateVideoStatusRequest
	): Promise<ApiResponse<UpdateVideoStatusResponse>> {
		return this.post<UpdateVideoStatusResponse>(`/videos/${id}/update-status`, request);
	}

	/**
	 * 检查是否需要初始设置
	 */
	async checkInitialSetup(): Promise<ApiResponse<InitialSetupCheckResponse>> {
		return this.get<InitialSetupCheckResponse>('/setup/check');
	}

	/**
	 * 更新B站登录凭证
	 * @param params 凭证参数
	 */
	async updateCredential(
		params: UpdateCredentialRequest
	): Promise<ApiResponse<UpdateCredentialResponse>> {
		return this.put<UpdateCredentialResponse>('/credential', params);
	}

	/**
	 * 设置API Token（初始设置时使用）
	 * @param token API Token
	 */
	async setupAuthToken(token: string): Promise<ApiResponse<{ success: boolean; message: string }>> {
		return this.post<{ success: boolean; message: string }>('/setup/auth-token', {
			auth_token: token
		});
	}

	/**
	 * 获取任务控制状态
	 */
	async getTaskControlStatus(): Promise<ApiResponse<TaskControlStatusResponse>> {
		return this.get<TaskControlStatusResponse>('/task-control/status');
	}

	/**
	 * 暂停所有扫描和下载任务
	 */
	async pauseScanning(): Promise<ApiResponse<TaskControlResponse>> {
		return this.post<TaskControlResponse>('/task-control/pause');
	}

	/**
	 * 恢复所有扫描和下载任务
	 */
	async resumeScanning(): Promise<ApiResponse<TaskControlResponse>> {
		return this.post<TaskControlResponse>('/task-control/resume');
	}

	/**
	 * 立即刷新任务（触发新一轮扫描，无需等待下一次定时触发）
	 */
	async refreshScanning(): Promise<ApiResponse<TaskControlResponse>> {
		return this.post<TaskControlResponse>('/task-control/refresh');
	}

	/**
	 * 获取视频播放信息（在线播放用）
	 * @param videoId 视频ID或分页ID
	 */
	async getVideoPlayInfo(videoId: string | number): Promise<ApiResponse<VideoPlayInfoResponse>> {
		return this.get<VideoPlayInfoResponse>(`/videos/${videoId}/play-info`);
	}

	/**
	 * 获取视频BVID信息（用于构建B站链接）
	 * @param videoId 视频ID或分页ID
	 */
	async getVideoBvid(videoId: string | number): Promise<ApiResponse<VideoBvidResponse>> {
		return this.get<VideoBvidResponse>(`/videos/${videoId}/bvid`);
	}

	/**
	 * 获取代理视频流URL
	 * @param streamUrl 原始视频流URL
	 */
	getProxyStreamUrl(streamUrl: string, options?: { transmux?: boolean }): string {
		const encodedUrl = encodeURIComponent(streamUrl);
		const transmuxParam = options?.transmux ? '&transmux=1' : '';
		return `${this.baseURL}/videos/proxy-stream?url=${encodedUrl}${transmuxParam}`;
	}

	/**
	 * 获取UP主投稿列表
	 * @param params 查询参数
	 */
	async getSubmissionVideos(
		params: SubmissionVideosRequest
	): Promise<ApiResponse<SubmissionVideosResponse>> {
		const queryParams: Record<string, string | number> = {};
		if (typeof params.page === 'number') {
			queryParams.page = params.page;
		}
		if (typeof params.page_size === 'number') {
			queryParams.page_size = params.page_size;
		}

		// 如果有关键词，添加到查询参数
		if (params.keyword) {
			queryParams.keyword = params.keyword;
		}

		return this.get<SubmissionVideosResponse>(`/submission/${params.up_id}/videos`, queryParams);
	}

	/**
	 * 获取仪表盘数据
	 */
	async getDashboard(): Promise<ApiResponse<DashBoardResponse>> {
		return this.get<DashBoardResponse>('/dashboard');
	}

	/**
	 * 获取首页最新入库列表
	 */
	async getLatestIngests(limit: number = 10): Promise<ApiResponse<LatestIngestResponse>> {
		return this.get<LatestIngestResponse>('/ingest/latest', { limit });
	}

	/**
	 * 检查 beta 镜像是否有更新（用于角标提示）
	 */
	async getBetaImageUpdateStatus(): Promise<ApiResponse<BetaImageUpdateStatusResponse>> {
		return this.get<BetaImageUpdateStatusResponse>('/updates/beta');
	}

	/**
	 * 获取推送通知状态
	 */
	async getNotificationStatus(): Promise<
		ApiResponse<{
			configured: boolean;
			enabled: boolean;
			last_notification_time: string | null;
			total_notifications_sent: number;
			last_error: string | null;
		}>
	> {
		return this.get<{
			configured: boolean;
			enabled: boolean;
			last_notification_time: string | null;
			total_notifications_sent: number;
			last_error: string | null;
		}>('/notification/status');
	}

	/**
	 * 获取推送通知配置
	 */
	async getNotificationConfig(): Promise<
		ApiResponse<{
			active_channel: string;
			enable_scan_notifications: boolean;
			serverchan_key?: string;
			serverchan3_uid?: string;
			serverchan3_sendkey?: string;
			wecom_webhook_url?: string;
			wecom_msgtype: string;
			wecom_mention_all: boolean;
			wecom_mentioned_list?: string[];
			notification_min_videos: number;
			notification_timeout: number;
			notification_retry_count: number;
		}>
	> {
		return this.get<{
			active_channel: string;
			enable_scan_notifications: boolean;
			serverchan_key?: string;
			serverchan3_uid?: string;
			serverchan3_sendkey?: string;
			wecom_webhook_url?: string;
			wecom_msgtype: string;
			wecom_mention_all: boolean;
			wecom_mentioned_list?: string[];
			notification_min_videos: number;
			notification_timeout: number;
			notification_retry_count: number;
		}>('/config/notification');
	}

	/**
	 * 更新推送通知配置
	 */
	async updateNotificationConfig(config: {
		active_channel?: string;
		enable_scan_notifications?: boolean;
		serverchan_key?: string;
		serverchan3_uid?: string;
		serverchan3_sendkey?: string;
		wecom_webhook_url?: string;
		wecom_msgtype?: string;
		wecom_mention_all?: boolean;
		wecom_mentioned_list?: string[];
		notification_min_videos?: number;
	}): Promise<
		ApiResponse<{
			success: boolean;
			message: string;
		}>
	> {
		return this.post<{
			success: boolean;
			message: string;
		}>('/config/notification', config);
	}

	/**
	 * 测试推送通知
	 */
	async testNotification(message?: string): Promise<
		ApiResponse<{
			success: boolean;
			message: string;
		}>
	> {
		return this.post<{
			success: boolean;
			message: string;
		}>('/notification/test', { message });
	}
}

// 创建默认的 API 客户端实例
export const apiClient = new ApiClient();

// 导出 API 方法的便捷函数
export const api = {
	/**
	 * 获取所有视频来源
	 */
	getVideoSources: () => apiClient.getVideoSources(),

	/**
	 * 获取视频列表
	 */
	getVideos: (params?: VideosRequest) => apiClient.getVideos(params),

	/**
	 * 获取单个视频详情
	 */
	getVideo: (id: number) => apiClient.getVideo(id),

	/**
	 * 重置视频下载状态
	 */
	resetVideo: (id: number, force?: boolean) => apiClient.resetVideo(id, force),

	/**
	 * 批量重置所有视频下载状态
	 */
	resetAllVideos: (
		params?: {
			collection?: number;
			favorite?: number;
			submission?: number;
			bangumi?: number;
			watch_later?: number;
		},
		force?: boolean
	) => apiClient.resetAllVideos(params, force),

	/**
	 * 删除视频（软删除）
	 */
	deleteVideo: (id: number) => apiClient.deleteVideo(id),

	/**
	 * 选择性重置特定任务
	 */
	resetSpecificTasks: (
		taskIndexes: number[],
		params?: {
			collection?: number;
			favorite?: number;
			submission?: number;
			bangumi?: number;
			watch_later?: number;
		},
		force?: boolean
	) => apiClient.resetSpecificTasks(taskIndexes, params, force),

	/**
	 * 设置认证 token
	 */
	setAuthToken: (token: string) => apiClient.setAuthToken(token),

	/**
	 * 添加视频源
	 */
	addVideoSource: (params: AddVideoSourceRequest) => apiClient.addVideoSource(params),

	/**
	 * 删除视频源
	 */
	deleteVideoSource: (sourceType: string, id: number, deleteLocalFiles?: boolean) =>
		apiClient.deleteVideoSource(sourceType, id, deleteLocalFiles),

	/**
	 * 获取配置
	 */
	getConfig: () => apiClient.getConfig(),

	/**
	 * 更新配置
	 */
	updateConfig: (params: UpdateConfigRequest) => apiClient.updateConfig(params),

	/**
	 * 搜索B站内容
	 */
	searchBilibili: (params: SearchRequest) => apiClient.searchBilibili(params),

	/**
	 * 获取用户收藏夹列表
	 */
	getUserFavorites: () => apiClient.getUserFavorites(),

	/**
	 * 验证收藏夹ID
	 */
	validateFavorite: (fid: string) => apiClient.validateFavorite(fid),

	/**
	 * 获取指定UP主的收藏夹列表
	 */
	getUserFavoritesByUid: (uid: string) => apiClient.getUserFavoritesByUid(uid),

	/**
	 * 获取UP主的合集和系列列表
	 */
	getUserCollections: (mid: string, page?: number, pageSize?: number) =>
		apiClient.getUserCollections(mid, page, pageSize),

	/**
	 * 获取番剧季度信息
	 */
	getBangumiSeasons: (seasonId: string) => apiClient.getBangumiSeasons(seasonId),

	/**
	 * 获取现有番剧源列表（用于合并选择）
	 */
	getBangumiSourcesForMerge: () => apiClient.getBangumiSourcesForMerge(),

	/**
	 * 获取关注的UP主列表
	 */
	getUserFollowings: () => apiClient.getUserFollowings(),

	/**
	 * 获取订阅的合集列表
	 */
	getSubscribedCollections: () => apiClient.getSubscribedCollections(),

	/**
	 * 获取队列状态
	 */
	getQueueStatus: () => apiClient.getQueueStatus(),

	/**
	 * 取消队列中的待处理任务
	 */
	cancelQueueTask: (taskId: string) => apiClient.cancelQueueTask(taskId),

	/**
	 * 更新视频状态
	 */
	updateVideoStatus: (id: number, request: UpdateVideoStatusRequest) =>
		apiClient.updateVideoStatus(id, request),

	/**
	 * 更新视频源启用状态
	 */
	updateVideoSourceEnabled: (sourceType: string, id: number, enabled: boolean) =>
		apiClient.updateVideoSourceEnabled(sourceType, id, enabled),

	/**
	 * 更新视频源扫描已删除视频设置
	 */
	updateVideoSourceScanDeleted: (sourceType: string, id: number, scanDeleted: boolean) =>
		apiClient.updateVideoSourceScanDeleted(sourceType, id, scanDeleted),

	/**
	 * 更新视频源下载选项
	 */
	updateVideoSourceDownloadOptions: (
		sourceType: string,
		id: number,
		options: {
			audio_only?: boolean;
			audio_only_m4a_only?: boolean;
			flat_folder?: boolean;
			download_danmaku?: boolean;
			download_subtitle?: boolean;
			use_dynamic_api?: boolean;
			ai_rename?: boolean;
			ai_rename_video_prompt?: string;
			ai_rename_audio_prompt?: string;
			ai_rename_enable_multi_page?: boolean;
			ai_rename_enable_collection?: boolean;
			ai_rename_enable_bangumi?: boolean;
			ai_rename_rename_parent_dir?: boolean;
		}
	) => apiClient.updateVideoSourceDownloadOptions(sourceType, id, options),

	/**
	 * 重设视频源路径
	 */
	resetVideoSourcePath: (sourceType: string, id: number, params: ResetVideoSourcePathRequest) =>
		apiClient.resetVideoSourcePath(sourceType, id, params),

	/**
	 * 更新投稿源选中视频列表
	 */
	updateSubmissionSelectedVideos: (id: number, selectedVideos: string[]) =>
		apiClient.updateSubmissionSelectedVideos(id, selectedVideos),

	/**
	 * 获取视频源关键词过滤器
	 */
	getVideoSourceKeywordFilters: (sourceType: string, id: number) =>
		apiClient.getVideoSourceKeywordFilters(sourceType, id),

	/**
	 * 更新视频源关键词过滤器（双列表模式）
	 */
	updateVideoSourceKeywordFilters: (
		sourceType: string,
		id: number,
		blacklistKeywords: string[],
		whitelistKeywords: string[],
		caseSensitive: boolean = true
	) =>
		apiClient.updateVideoSourceKeywordFilters(
			sourceType,
			id,
			blacklistKeywords,
			whitelistKeywords,
			caseSensitive
		),

	/**
	 * 验证正则表达式
	 */
	validateRegex: (pattern: string) => apiClient.validateRegex(pattern),

	/**
	 * 清除AI对话历史缓存
	 */
	clearAiRenameCache: () => apiClient.clearAiRenameCache(),

	/**
	 * 清除指定视频源的AI对话历史缓存
	 */
	clearAiRenameCacheForSource: (sourceType: string, id: number) =>
		apiClient.clearAiRenameCacheForSource(sourceType, id),

	/**
	 * 批量AI重命名视频源下的历史文件
	 */
	aiRenameHistory: (
		sourceType: string,
		id: number,
		videoPrompt?: string,
		audioPrompt?: string,
		enableMultiPage?: boolean,
		enableCollection?: boolean,
		enableBangumi?: boolean,
		renameParentDir?: boolean
	) =>
		apiClient.aiRenameHistory(
			sourceType,
			id,
			videoPrompt,
			audioPrompt,
			enableMultiPage,
			enableCollection,
			enableBangumi,
			renameParentDir
		),

	/**
	 * 检查是否需要初始设置
	 */
	checkInitialSetup: () => apiClient.checkInitialSetup(),

	/**
	 * 更新B站登录凭证
	 */
	updateCredential: (params: UpdateCredentialRequest) => apiClient.updateCredential(params),

	/**
	 * 设置API Token（初始设置时使用）
	 */
	setupAuthToken: (token: string) => apiClient.setupAuthToken(token),

	/**
	 * 获取任务控制状态
	 */
	getTaskControlStatus: () => apiClient.getTaskControlStatus(),

	/**
	 * 暂停所有扫描和下载任务
	 */
	pauseScanning: () => apiClient.pauseScanning(),

	/**
	 * 恢复所有扫描和下载任务
	 */
	resumeScanning: () => apiClient.resumeScanning(),

	/**
	 * 立即刷新任务（触发新一轮扫描，无需等待下一次定时触发）
	 */
	refreshScanning: () => apiClient.refreshScanning(),

	/**
	 * 获取视频播放信息（在线播放用）
	 */
	getVideoPlayInfo: (videoId: string | number) => apiClient.getVideoPlayInfo(videoId),

	/**
	 * 获取视频BVID信息（用于构建B站链接）
	 */
	getVideoBvid: (videoId: string | number) => apiClient.getVideoBvid(videoId),

	/**
	 * 获取代理视频流URL
	 */
	getProxyStreamUrl: (streamUrl: string, options?: { transmux?: boolean }) =>
		apiClient.getProxyStreamUrl(streamUrl, options),

	/**
	 * 获取UP主投稿列表
	 */
	getSubmissionVideos: (params: SubmissionVideosRequest) => apiClient.getSubmissionVideos(params),

	/**
	 * 获取仪表盘数据
	 */
	getDashboard: () => apiClient.getDashboard(),

	/**
	 * 获取首页最新入库列表
	 */
	getLatestIngests: (limit: number = 10) => apiClient.getLatestIngests(limit),

	/**
	 * 检查 beta 镜像是否有更新（用于角标提示）
	 */
	getBetaImageUpdateStatus: () => apiClient.getBetaImageUpdateStatus(),

	/**
	 * 获取推送通知状态
	 */
	getNotificationStatus: () => apiClient.getNotificationStatus(),

	/**
	 * 获取推送通知配置
	 */
	getNotificationConfig: () => apiClient.getNotificationConfig(),

	/**
	 * 更新推送通知配置
	 */
	updateNotificationConfig: (config: {
		active_channel?: string;
		enable_scan_notifications?: boolean;
		serverchan_key?: string;
		serverchan3_uid?: string;
		serverchan3_sendkey?: string;
		wecom_webhook_url?: string;
		wecom_msgtype?: string;
		wecom_mention_all?: boolean;
		wecom_mentioned_list?: string[];
		notification_min_videos?: number;
	}) => apiClient.updateNotificationConfig(config),

	/**
	 * 测试推送通知
	 */
	testNotification: (message?: string) => apiClient.testNotification(message),

	/**
	 * 订阅系统信息WebSocket事件
	 */
	subscribeToSysInfo: (callback: (data: SysInfo) => void) => wsManager.subscribeToSysInfo(callback),

	/**
	 * 订阅任务状态WebSocket事件
	 */
	subscribeToTasks: (callback: (data: TaskStatus) => void) => wsManager.subscribeToTasks(callback)
};

// 默认导出
export default api;
