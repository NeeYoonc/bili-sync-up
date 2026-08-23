import type {
	ApiResponse,
	VideoSourcesResponse,
	FilterOption,
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
	FilenamePreviewRequest,
	FilenamePreviewResponse,
	RefreshDanmakuResponse,
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
	UpdateVideoSourceScanDeletedResponse,
	ResetVideoSourcePathRequest,
	ResetVideoSourcePathResponse,
	RetryChargeVideosResponse,
	UpdateSubmissionSelectedVideosResponse,
	UpdateKeywordFiltersResponse,
	GetKeywordFiltersResponse,
	ValidateRegexResponse,
	UpdateCredentialRequest,
	UpdateCredentialResponse,
	CredentialRefreshTestRequest,
	CredentialRefreshTestResponse,
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
	LatestIngestResponse,
	BetaImageUpdateStatusResponse,
	YouTubeLoginResponse,
	YouTubeStatusResponse,
	DouyinStatusResponse,
	TikTokSecUidStatusResponse,
	TikTokStatusResponse,
	TestProxyResponse,
	DatabaseStatusResponse,
	DatabaseMaintenanceAction,
	DatabaseMaintenanceResponse,
	DatabaseBackupInfo,
	DatabaseBackupListResponse,
	DatabaseRestoreResponse,
	YouTubeSearchRequest,
	YouTubeSearchResponse,
	YouTubeSourceVideosRequest,
	YouTubeSource,
	YouTubeVideo,
	CreateYouTubeSourceRequest,
	UpdateYouTubeSourceRequest,
	YouTubeQueueStatusResponse
} from './types';
import { ErrorType } from './types';
import { wsManager } from './ws';

type ResetSpecificTasksOptions =
	| number[]
	| {
			taskIndexes?: number[];
			videoTaskIndexes?: number[];
			pageTaskIndexes?: number[];
	  };

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

		const headers: Record<string, string> = {
			...this.defaultHeaders,
			...(options.headers as Record<string, string> | undefined)
		};
		if (typeof FormData !== 'undefined' && options.body instanceof FormData) {
			delete headers['Content-Type'];
		}
		const config: RequestInit = {
			headers,
			...options
		};

		try {
			const response = await fetch(url, config);

			if (!response.ok) {
				// 尝试读取响应体获取详细错误信息
				let errorMessage = `HTTP error! status: ${response.status}`;
				try {
					const errorData = (await response.json()) as {
						data?: string | { message?: string };
						message?: string;
						error?: string;
					};
					if (errorData && typeof errorData.data === 'string') {
						errorMessage = errorData.data;
					} else if (
						errorData &&
						typeof errorData.data === 'object' &&
						errorData.data &&
						typeof errorData.data.message === 'string'
					) {
						errorMessage = errorData.data.message;
					} else if (errorData && typeof errorData.message === 'string') {
						errorMessage = errorData.message;
					} else if (errorData && typeof errorData.error === 'string') {
						errorMessage = errorData.error;
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

	/** 获取本地 yt-dlp、YouTube 登录和下载任务状态。 */
	async getYouTubeStatus(): Promise<ApiResponse<YouTubeStatusResponse>> {
		return this.get<YouTubeStatusResponse>('/youtube/status');
	}

	async searchYouTube(params: YouTubeSearchRequest): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/youtube/search', { ...params });
	}

	async getYouTubeSourceVideos(
		params: YouTubeSourceVideosRequest
	): Promise<ApiResponse<SubmissionVideosResponse>> {
		return this.get<SubmissionVideosResponse>('/youtube/source-videos', { ...params });
	}

	/** 列出指定 YouTube 频道的全部播放列表（添加“播放列表/收藏”来源时选择）。 */
	async getYouTubeChannelPlaylists(url: string): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/youtube/channel-playlists', { url });
	}

	/** 接收电脑端登录助手或手动选择的 Netscape cookies.txt。 */
	async importYouTubeCookies(cookies: string): Promise<ApiResponse<YouTubeLoginResponse>> {
		return this.post<YouTubeLoginResponse>('/youtube/cookies', { cookies });
	}

	async getYouTubeSources(): Promise<ApiResponse<YouTubeSource[]>> {
		return this.get<YouTubeSource[]>('/youtube/sources');
	}
	async createYouTubeSource(
		request: CreateYouTubeSourceRequest
	): Promise<ApiResponse<YouTubeSource>> {
		return this.post<YouTubeSource>('/youtube/sources', request);
	}
	async setYouTubeSourceEnabled(id: number, enabled: boolean): Promise<ApiResponse<YouTubeSource>> {
		return this.put<YouTubeSource>(`/youtube/sources/${id}/enabled`, { enabled });
	}
	async updateYouTubeSource(
		id: number,
		request: UpdateYouTubeSourceRequest
	): Promise<ApiResponse<YouTubeSource>> {
		return this.put<YouTubeSource>(`/youtube/sources/${id}`, request);
	}
	async resetYouTubeSourcePath(id: number, new_path: string): Promise<ApiResponse<YouTubeSource>> {
		return this.post<YouTubeSource>(`/youtube/sources/${id}/reset-path`, { new_path });
	}
	async deleteYouTubeSource(id: number, deleteLocalFiles = false): Promise<ApiResponse<boolean>> {
		return this.delete<boolean>(`/youtube/sources/${id}?delete_local_files=${deleteLocalFiles}`);
	}
	async getYouTubeVideos(): Promise<ApiResponse<YouTubeVideo[]>> {
		return this.get<YouTubeVideo[]>('/youtube/videos');
	}
	async retryYouTubeVideo(id: number): Promise<ApiResponse<boolean>> {
		return this.post<boolean>(`/youtube/videos/${id}/retry`);
	}
	async getYouTubeQueueStatus(): Promise<ApiResponse<YouTubeQueueStatusResponse>> {
		return this.get<YouTubeQueueStatusResponse>('/youtube/queue-status');
	}

	async getDouyinStatus(): Promise<ApiResponse<DouyinStatusResponse>> {
		return this.get<DouyinStatusResponse>('/douyin/status');
	}
	async importDouyinCookies(cookies: string): Promise<ApiResponse<YouTubeLoginResponse>> {
		return this.post<YouTubeLoginResponse>('/douyin/cookies', { cookies });
	}
	async searchDouyin(keyword: string): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/douyin/search', { keyword });
	}
	async searchTikTok(keyword: string): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/tiktok/search', { keyword });
	}
	async getTikTokStatus(): Promise<ApiResponse<TikTokStatusResponse>> {
		return this.get<TikTokStatusResponse>('/tiktok/status');
	}
	async importTikTokCookies(cookies: string): Promise<ApiResponse<YouTubeLoginResponse>> {
		return this.post<YouTubeLoginResponse>('/tiktok/cookies', { cookies });
	}
	async getTikTokSecUid(): Promise<ApiResponse<TikTokSecUidStatusResponse>> {
		return this.get<TikTokSecUidStatusResponse>('/tiktok/secuid');
	}
	async setTikTokSecUid(secUid: string): Promise<ApiResponse<{ success: boolean; message: string }>> {
		return this.put<{ success: boolean; message: string }>('/tiktok/secuid', { sec_uid: secUid });
	}
	async getTikTokSourceVideos(params: {
		url?: string;
		source_type: string;
		page?: number;
		page_size?: number;
		keyword?: string;
	}): Promise<ApiResponse<SubmissionVideosResponse>> {
		return this.get<SubmissionVideosResponse>('/tiktok/source-videos', { ...params });
	}
	async getTikTokPlaylists(url: string): Promise<ApiResponse<YouTubeSearchResponse>> {
		// url 为空时后端读取当前登录账号自己的收藏夹，无需主页链接
		return this.get<YouTubeSearchResponse>('/tiktok/playlists', { url: url.trim() || undefined });
	}
	async getTikTokFollowings(): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/tiktok/followings');
	}
	async getDouyinFollowings(): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/douyin/followings');
	}
	async getDouyinCatalog(
		source_type: string,
		keyword?: string
	): Promise<ApiResponse<YouTubeSearchResponse>> {
		return this.get<YouTubeSearchResponse>('/douyin/catalog', { source_type, keyword });
	}
	async getDouyinSourceVideos(params: {
		url: string;
		source_type?: string;
		page?: number;
		page_size?: number;
		keyword?: string;
	}): Promise<ApiResponse<SubmissionVideosResponse>> {
		return this.get<SubmissionVideosResponse>('/douyin/source-videos', { ...params });
	}
	async getDouyinSources(): Promise<ApiResponse<YouTubeSource[]>> {
		return this.get<YouTubeSource[]>('/douyin/sources');
	}
	async createDouyinSource(
		request: CreateYouTubeSourceRequest
	): Promise<ApiResponse<YouTubeSource>> {
		return this.post<YouTubeSource>('/douyin/sources', request);
	}
	async setDouyinSourceEnabled(id: number, enabled: boolean): Promise<ApiResponse<YouTubeSource>> {
		return this.put<YouTubeSource>(`/douyin/sources/${id}/enabled`, { enabled });
	}
	async updateDouyinSource(
		id: number,
		request: UpdateYouTubeSourceRequest
	): Promise<ApiResponse<YouTubeSource>> {
		return this.put<YouTubeSource>(`/douyin/sources/${id}`, request);
	}
	async resetDouyinSourcePath(id: number, new_path: string): Promise<ApiResponse<YouTubeSource>> {
		return this.post<YouTubeSource>(`/douyin/sources/${id}/reset-path`, { new_path });
	}
	async deleteDouyinSource(id: number, deleteLocalFiles = false): Promise<ApiResponse<boolean>> {
		return this.delete<boolean>(`/douyin/sources/${id}?delete_local_files=${deleteLocalFiles}`);
	}

	async getTikTokSources(): Promise<ApiResponse<YouTubeSource[]>> {
		return this.get<YouTubeSource[]>('/tiktok/sources');
	}
	async createTikTokSource(
		request: CreateYouTubeSourceRequest
	): Promise<ApiResponse<YouTubeSource>> {
		return this.post<YouTubeSource>('/tiktok/sources', request);
	}
	async setTikTokSourceEnabled(id: number, enabled: boolean): Promise<ApiResponse<YouTubeSource>> {
		return this.put<YouTubeSource>(`/tiktok/sources/${id}/enabled`, { enabled });
	}
	async updateTikTokSource(
		id: number,
		request: UpdateYouTubeSourceRequest
	): Promise<ApiResponse<YouTubeSource>> {
		return this.put<YouTubeSource>(`/tiktok/sources/${id}`, request);
	}
	async resetTikTokSourcePath(id: number, new_path: string): Promise<ApiResponse<YouTubeSource>> {
		return this.post<YouTubeSource>(`/tiktok/sources/${id}/reset-path`, { new_path });
	}
	async deleteTikTokSource(id: number, deleteLocalFiles = false): Promise<ApiResponse<boolean>> {
		return this.delete<boolean>(`/tiktok/sources/${id}?delete_local_files=${deleteLocalFiles}`);
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
	async getVideo(id: string | number): Promise<ApiResponse<VideoResponse>> {
		return this.get<VideoResponse>(`/videos/${id}`);
	}

	async refreshVideoDanmaku(id: number): Promise<ApiResponse<RefreshDanmakuResponse>> {
		return this.post<RefreshDanmakuResponse>(`/videos/${id}/refresh-danmaku`);
	}

	async refreshPageDanmaku(id: number): Promise<ApiResponse<RefreshDanmakuResponse>> {
		return this.post<RefreshDanmakuResponse>(`/pages/${id}/refresh-danmaku`);
	}

	/**
	 * 重置视频下载状态
	 * @param id 视频 ID
	 * @param force 是否强制重置
	 */
	async resetVideo(
		id: string | number,
		force: boolean = false
	): Promise<ApiResponse<ResetVideoResponse>> {
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
	async deleteVideo(id: string | number): Promise<ApiResponse<DeleteVideoResponse>> {
		return this.delete<DeleteVideoResponse>(`/videos/${id}`);
	}

	/**
	 * 选择性重置特定任务
	 * @param taskIndexes 要重置的任务索引列表；也可分别传 videoTaskIndexes / pageTaskIndexes
	 * @param params 可选的查询参数，用于筛选要重置的视频（与 /api/videos 参数保持一致的子集）
	 * @param force 是否强制重置（包括已完成的任务）
	 */
	async resetSpecificTasks(
		taskIndexes: ResetSpecificTasksOptions,
		params?: VideosRequest,
		force: boolean = false
	): Promise<ApiResponse<ResetAllVideosResponse>> {
		const taskPayload = Array.isArray(taskIndexes)
			? { task_indexes: taskIndexes }
			: {
					task_indexes: taskIndexes.taskIndexes ?? [],
					video_task_indexes: taskIndexes.videoTaskIndexes ?? [],
					page_task_indexes: taskIndexes.pageTaskIndexes ?? []
				};
		const requestBody = {
			...taskPayload,
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
		id: string | number,
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
		if (sourceType === 'youtube' || sourceType === 'douyin' || sourceType === 'tiktok') {
			const result =
				sourceType === 'tiktok'
					? await this.setTikTokSourceEnabled(id, enabled)
					: sourceType === 'douyin'
						? await this.setDouyinSourceEnabled(id, enabled)
						: await this.setYouTubeSourceEnabled(id, enabled);
			return {
				status_code: result.status_code,
				data: {
					success: true,
					source_id: id,
					source_type: sourceType,
					enabled: result.data.enabled,
					message: enabled ? '视频源已启用' : '视频源已禁用'
				}
			};
		}
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
	): Promise<ApiResponse<UpdateVideoSourceScanDeletedResponse>> {
		return this.put<UpdateVideoSourceScanDeletedResponse>(
			`/video-sources/${sourceType}/${id}/scan-deleted`,
			{ scan_deleted_videos: scanDeleted }
		);
	}

	/**
	 * 更新视频源一次性扫描已删除视频设置
	 * @param sourceType 视频源类型
	 * @param id 视频源ID
	 * @param scanDeletedOnce 是否本轮临时扫描已删除视频
	 */
	async updateVideoSourceScanDeletedOnce(
		sourceType: string,
		id: number,
		scanDeletedOnce: boolean
	): Promise<ApiResponse<UpdateVideoSourceScanDeletedResponse>> {
		return this.put<UpdateVideoSourceScanDeletedResponse>(
			`/video-sources/${sourceType}/${id}/scan-deleted-once`,
			{ scan_deleted_videos_once: scanDeletedOnce }
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
			split_chapters_after_download?: boolean;
			download_charge_videos?: boolean;
			download_danmaku?: boolean;
			download_subtitle?: boolean;
			download_ai_subtitle?: boolean;
			ai_subtitle_language?: string;
			use_dynamic_api?: boolean;
			collection_aggregate_enabled?: boolean;
			ai_rename?: boolean;
			ai_rename_video_prompt?: string;
			ai_rename_audio_prompt?: string;
			ai_rename_enable_multi_page?: boolean;
			ai_rename_enable_collection?: boolean;
			ai_rename_enable_bangumi?: boolean;
			ai_rename_rename_parent_dir?: boolean;
			filter_option?: FilterOption | null;
		}
	): Promise<
		ApiResponse<{
			success: boolean;
			source_id: number;
			source_type: string;
			audio_only: boolean;
			audio_only_m4a_only: boolean;
			flat_folder: boolean;
			split_chapters_after_download: boolean;
			download_charge_videos: boolean;
			download_danmaku: boolean;
			download_subtitle: boolean;
			download_ai_subtitle: boolean;
			ai_subtitle_language: string;
			ai_rename: boolean;
			ai_rename_video_prompt: string;
			ai_rename_audio_prompt: string;
			ai_rename_enable_multi_page: boolean;
			ai_rename_enable_collection: boolean;
			ai_rename_enable_bangumi: boolean;
			ai_rename_rename_parent_dir: boolean;
			use_dynamic_api: boolean;
			collection_aggregate_enabled: boolean;
			collection_aggregate_season_number?: number;
			filter_option?: FilterOption | null;
			message: string;
		}>
	> {
		if (sourceType === 'youtube' || sourceType === 'douyin' || sourceType === 'tiktok') {
			const update =
				sourceType === 'tiktok'
					? this.updateTikTokSource.bind(this)
					: sourceType === 'douyin'
						? this.updateDouyinSource.bind(this)
						: this.updateYouTubeSource.bind(this);
			const result = await update(id, {
				audio_only: options.audio_only,
				audio_only_m4a_only: options.audio_only_m4a_only,
				flat_folder: options.flat_folder,
				download_danmaku: options.download_danmaku,
				download_subtitle: options.download_subtitle,
				ai_subtitle_language: options.ai_subtitle_language,
				filter_option: options.filter_option,
				inherit_filter_option: options.filter_option === null
			});
			const source = result.data;
			return {
				status_code: result.status_code,
				data: {
					success: true,
					source_id: id,
					source_type: sourceType,
					audio_only: source.audio_only,
					audio_only_m4a_only: source.audio_only_m4a_only,
					flat_folder: source.flat_folder,
					split_chapters_after_download: false,
					download_charge_videos: false,
					download_danmaku: source.download_danmaku,
					download_subtitle: source.download_subtitle,
					download_ai_subtitle: source.download_subtitle,
					ai_subtitle_language: source.ai_subtitle_language,
					ai_rename: false,
					ai_rename_video_prompt: '',
					ai_rename_audio_prompt: '',
					ai_rename_enable_multi_page: false,
					ai_rename_enable_collection: false,
					ai_rename_enable_bangumi: false,
					ai_rename_rename_parent_dir: false,
					use_dynamic_api: false,
					collection_aggregate_enabled: false,
					filter_option: source.filter_option,
					message: `${sourceType === 'douyin' ? '抖音' : 'YouTube'}视频源下载设置已更新`
				}
			};
		}
		return this.put<{
			success: boolean;
			source_id: number;
			source_type: string;
			audio_only: boolean;
			audio_only_m4a_only: boolean;
			flat_folder: boolean;
			split_chapters_after_download: boolean;
			download_charge_videos: boolean;
			download_danmaku: boolean;
			download_subtitle: boolean;
			download_ai_subtitle: boolean;
			ai_subtitle_language: string;
			ai_rename: boolean;
			ai_rename_video_prompt: string;
			ai_rename_audio_prompt: string;
			ai_rename_enable_multi_page: boolean;
			ai_rename_enable_collection: boolean;
			ai_rename_enable_bangumi: boolean;
			ai_rename_rename_parent_dir: boolean;
			use_dynamic_api: boolean;
			collection_aggregate_enabled: boolean;
			collection_aggregate_season_number?: number;
			filter_option?: FilterOption | null;
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
		if (sourceType === 'youtube' || sourceType === 'douyin' || sourceType === 'tiktok') {
			const before = (
				sourceType === 'tiktok'
					? await this.getTikTokSources()
					: sourceType === 'douyin'
						? await this.getDouyinSources()
						: await this.getYouTubeSources()
			).data.find((source) => source.id === id);
			const result =
				sourceType === 'tiktok'
					? await this.resetTikTokSourcePath(id, params.new_path)
					: sourceType === 'douyin'
						? await this.resetDouyinSourcePath(id, params.new_path)
						: await this.resetYouTubeSourcePath(id, params.new_path);
			return {
				status_code: result.status_code,
				data: {
					success: true,
					source_id: id,
					source_type: sourceType,
					old_path: before?.path ?? '',
					new_path: result.data.path,
					moved_files_count: 0,
					updated_videos_count: result.data.completed_count,
					cleaned_folders_count: 0,
					message: `${sourceType === 'douyin' ? '抖音' : 'YouTube'}视频源路径已按现有目录规则更新`
				}
			};
		}
		return this.post<ResetVideoSourcePathResponse>(
			`/video-sources/${sourceType}/${id}/reset-path`,
			params
		);
	}

	/**
	 * 重试视频源下已识别的充电视频
	 * @param sourceType 视频源类型，支持除 bangumi 外的视频源
	 * @param id 视频源ID
	 */
	async retryChargeVideosForSource(
		sourceType: string,
		id: number
	): Promise<ApiResponse<RetryChargeVideosResponse>> {
		return this.post<RetryChargeVideosResponse>(
			`/video-sources/${sourceType}/${id}/retry-charge-videos`
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
		if (sourceType === 'youtube' || sourceType === 'douyin' || sourceType === 'tiktok') {
			const response =
				sourceType === 'tiktok'
					? await this.getTikTokSources()
					: sourceType === 'douyin'
						? await this.getDouyinSources()
						: await this.getYouTubeSources();
			const source = response.data.find((item) => item.id === id);
			if (!source)
				throw new Error(
					`${sourceType === 'tiktok' ? 'TikTok' : sourceType === 'douyin' ? '抖音' : 'YouTube'}视频源不存在`
				);
			return {
				status_code: response.status_code,
				data: {
					success: true,
					source_id: id,
					source_type: sourceType,
					blacklist_keywords: source.blacklist_keywords,
					whitelist_keywords: source.whitelist_keywords,
					case_sensitive: source.case_sensitive,
					min_duration_seconds: source.min_duration_seconds ?? undefined,
					max_duration_seconds: source.max_duration_seconds ?? undefined,
					published_after: source.published_after ?? undefined,
					published_before: source.published_before ?? undefined,
					keyword_filters: []
				}
			};
		}
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
		caseSensitive: boolean = true,
		minDurationSeconds?: number | null,
		maxDurationSeconds?: number | null,
		publishedAfter?: string,
		publishedBefore?: string
	): Promise<ApiResponse<UpdateKeywordFiltersResponse>> {
		if (sourceType === 'youtube' || sourceType === 'douyin' || sourceType === 'tiktok') {
			const update =
				sourceType === 'tiktok'
					? this.updateTikTokSource.bind(this)
					: sourceType === 'douyin'
						? this.updateDouyinSource.bind(this)
						: this.updateYouTubeSource.bind(this);
			const response = await update(id, {
				blacklist_keywords: blacklistKeywords,
				whitelist_keywords: whitelistKeywords,
				case_sensitive: caseSensitive,
				min_duration_seconds: minDurationSeconds ?? null,
				max_duration_seconds: maxDurationSeconds ?? null,
				published_after: publishedAfter || null,
				published_before: publishedBefore || null
			});
			return {
				status_code: response.status_code,
				data: {
					success: true,
					source_id: id,
					source_type: sourceType,
					blacklist_count: response.data.blacklist_keywords.length,
					whitelist_count: response.data.whitelist_keywords.length,
					message: `${sourceType === 'tiktok' ? 'TikTok' : sourceType === 'douyin' ? '抖音' : 'YouTube'}视频源过滤设置已更新`
				}
			};
		}
		return this.put<UpdateKeywordFiltersResponse>(
			`/video-sources/${sourceType}/${id}/keyword-filters`,
			{
				blacklist_keywords: blacklistKeywords,
				whitelist_keywords: whitelistKeywords,
				case_sensitive: caseSensitive,
				min_duration_seconds: minDurationSeconds ?? undefined,
				max_duration_seconds: maxDurationSeconds ?? undefined,
				published_after: publishedAfter || undefined,
				published_before: publishedBefore || undefined
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
	 * 预览文件命名模板
	 * @param params 当前命名模板配置
	 */
	async previewFilenameTemplates(
		params: FilenamePreviewRequest
	): Promise<ApiResponse<FilenamePreviewResponse>> {
		return this.post<FilenamePreviewResponse>('/config/name-preview', params);
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
		id: string | number,
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

	async testCredentialRefresh(force = false): Promise<ApiResponse<CredentialRefreshTestResponse>> {
		const request: CredentialRefreshTestRequest = { force };
		return this.post<CredentialRefreshTestResponse>('/credential/test-refresh', request);
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
	async getVideoPlayInfo(
		videoId: string | number,
		options?: { refresh?: boolean }
	): Promise<ApiResponse<VideoPlayInfoResponse>> {
		const params =
			typeof options?.refresh === 'boolean'
				? {
						refresh: options.refresh
					}
				: undefined;
		return this.get<VideoPlayInfoResponse>(`/videos/${videoId}/play-info`, params);
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
	async getLatestIngests(
		limit: number = 10,
		platform: string = 'all'
	): Promise<ApiResponse<LatestIngestResponse>> {
		return this.get<LatestIngestResponse>('/ingest/latest', { limit, platform });
	}

	/**
	 * 获取首页最近处理列表
	 */
	async getRecentIngests(
		limit: number = 10,
		platform: string = 'all'
	): Promise<ApiResponse<LatestIngestResponse>> {
		return this.get<LatestIngestResponse>('/ingest/recent', { limit, platform });
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
			webhook_url?: string;
			webhook_bearer_token?: string;
			webhook_custom_headers?: string;
			webhook_format?: string;
			webhook_custom_body?: string;
			webhook_synology_chat_template?: string;
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
			webhook_url?: string;
			webhook_bearer_token?: string;
			webhook_custom_headers?: string;
			webhook_format?: string;
			webhook_custom_body?: string;
			webhook_synology_chat_template?: string;
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
		webhook_url?: string;
		webhook_bearer_token?: string;
		webhook_custom_headers?: string;
		webhook_format?: string;
		webhook_custom_body?: string;
		webhook_synology_chat_template?: string;
		notification_min_videos?: number;
	}): Promise<ApiResponse<string>> {
		return this.post<string>('/config/notification', config);
	}

	/**
	 * 测试推送通知
	 */
	async testNotification(
		params?: {
			custom_message?: string;
			active_channel?: string;
			serverchan_key?: string;
			serverchan3_uid?: string;
			serverchan3_sendkey?: string;
			wecom_webhook_url?: string;
			wecom_msgtype?: string;
			wecom_mention_all?: boolean;
			wecom_mentioned_list?: string[];
			webhook_url?: string;
			webhook_bearer_token?: string;
			webhook_custom_headers?: string;
			webhook_format?: string;
			webhook_custom_body?: string;
			webhook_synology_chat_template?: string;
		}
	): Promise<
		ApiResponse<{
			success: boolean;
			message: string;
		}>
	> {
		return this.post<{
			success: boolean;
			message: string;
		}>('/notification/test', params ?? {});
	}

	/**
	 * 测试外源网络代理到谷歌官网的连通性
	 */
	/**
	 * 获取数据库状态概览（设置页 → 数据库管理）
	 */
	async getDatabaseStatus(): Promise<ApiResponse<DatabaseStatusResponse>> {
		return this.get<DatabaseStatusResponse>('/database/status');
	}

	/**
	 * 执行数据库维护操作（设置页 → 数据库管理）
	 */
	async runDatabaseMaintenance(
		action: DatabaseMaintenanceAction
	): Promise<ApiResponse<DatabaseMaintenanceResponse>> {
		return this.post<DatabaseMaintenanceResponse>('/database/maintenance', { action });
	}

	/** 获取数据库备份列表 */
	async getDatabaseBackups(): Promise<ApiResponse<DatabaseBackupListResponse>> {
		return this.get<DatabaseBackupListResponse>('/database/backups');
	}

	/** 安排数据库恢复（重启后生效） */
	async restoreDatabase(backupFile: string): Promise<ApiResponse<DatabaseRestoreResponse>> {
		return this.post<DatabaseRestoreResponse>('/database/restore', { backup_file: backupFile });
	}

	async uploadRestoreBackup(file: File, filename?: string): Promise<ApiResponse<DatabaseRestoreResponse>> {
		const form = new FormData();
		form.append('file', file, filename ?? file.name);
		return this.request<DatabaseRestoreResponse>('/database/restore-upload', {
			method: 'POST',
			body: form
		});
	}

	async testProxy(proxy?: string): Promise<ApiResponse<TestProxyResponse>> {
		return this.post<TestProxyResponse>('/proxy/test', proxy !== undefined ? { proxy } : {});
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

	/** 获取本地 YouTube 下载器、登录和任务状态。 */
	getYouTubeStatus: () => apiClient.getYouTubeStatus(),

	/** 从电脑端登录助手或 cookies.txt 导入 YouTube 登录状态。 */
	importYouTubeCookies: (cookies: string) => apiClient.importYouTubeCookies(cookies),

	getYouTubeSources: () => apiClient.getYouTubeSources(),
	createYouTubeSource: (request: CreateYouTubeSourceRequest) =>
		apiClient.createYouTubeSource(request),
	setYouTubeSourceEnabled: (id: number, enabled: boolean) =>
		apiClient.setYouTubeSourceEnabled(id, enabled),
	updateYouTubeSource: (id: number, request: UpdateYouTubeSourceRequest) =>
		apiClient.updateYouTubeSource(id, request),
	resetYouTubeSourcePath: (id: number, newPath: string) =>
		apiClient.resetYouTubeSourcePath(id, newPath),
	deleteYouTubeSource: (id: number, deleteLocalFiles = false) =>
		apiClient.deleteYouTubeSource(id, deleteLocalFiles),
	getYouTubeVideos: () => apiClient.getYouTubeVideos(),
	getYouTubeChannelPlaylists: (url: string) => apiClient.getYouTubeChannelPlaylists(url),
	retryYouTubeVideo: (id: number) => apiClient.retryYouTubeVideo(id),
	getYouTubeQueueStatus: () => apiClient.getYouTubeQueueStatus(),
	getDouyinStatus: () => apiClient.getDouyinStatus(),
	importDouyinCookies: (cookies: string) => apiClient.importDouyinCookies(cookies),
	getDouyinFollowings: () => apiClient.getDouyinFollowings(),
	getDouyinCatalog: (sourceType: string, keyword?: string) =>
		apiClient.getDouyinCatalog(sourceType, keyword),
	getDouyinSources: () => apiClient.getDouyinSources(),
	createDouyinSource: (request: CreateYouTubeSourceRequest) =>
		apiClient.createDouyinSource(request),
	setDouyinSourceEnabled: (id: number, enabled: boolean) =>
		apiClient.setDouyinSourceEnabled(id, enabled),
	updateDouyinSource: (id: number, request: UpdateYouTubeSourceRequest) =>
		apiClient.updateDouyinSource(id, request),
	resetDouyinSourcePath: (id: number, newPath: string) =>
		apiClient.resetDouyinSourcePath(id, newPath),
	deleteDouyinSource: (id: number, deleteLocalFiles = false) =>
		apiClient.deleteDouyinSource(id, deleteLocalFiles),

	/**
	 * 获取视频列表
	 */
	getVideos: (params?: VideosRequest) => apiClient.getVideos(params),

	/**
	 * 获取单个视频详情
	 */
	getVideo: (id: string | number) => apiClient.getVideo(id),

	refreshVideoDanmaku: (id: number) => apiClient.refreshVideoDanmaku(id),

	refreshPageDanmaku: (id: number) => apiClient.refreshPageDanmaku(id),

	/**
	 * 重置视频下载状态
	 */
	resetVideo: (id: string | number, force?: boolean) => apiClient.resetVideo(id, force),

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
	deleteVideo: (id: string | number) => apiClient.deleteVideo(id),

	/**
	 * 选择性重置特定任务
	 */
	resetSpecificTasks: (
		taskIndexes: ResetSpecificTasksOptions,
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
	 * 预览文件命名模板
	 */
	previewFilenameTemplates: (params: FilenamePreviewRequest) =>
		apiClient.previewFilenameTemplates(params),

	/**
	 * 搜索B站内容
	 */
	searchBilibili: (params: SearchRequest) => apiClient.searchBilibili(params),
	searchYouTube: (params: YouTubeSearchRequest) => apiClient.searchYouTube(params),
	getYouTubeSourceVideos: (params: YouTubeSourceVideosRequest) =>
		apiClient.getYouTubeSourceVideos(params),
	searchDouyin: (keyword: string) => apiClient.searchDouyin(keyword),
	getTikTokStatus: () => apiClient.getTikTokStatus(),
	getTikTokSecUid: () => apiClient.getTikTokSecUid(),
	setTikTokSecUid: (secUid: string) => apiClient.setTikTokSecUid(secUid),
	importTikTokCookies: (cookies: string) => apiClient.importTikTokCookies(cookies),
	getTikTokSourceVideos: (params: {
		url?: string;
		source_type: string;
		page?: number;
		page_size?: number;
		keyword?: string;
	}) => apiClient.getTikTokSourceVideos(params),
	getTikTokPlaylists: (url: string) => apiClient.getTikTokPlaylists(url),
	getTikTokFollowings: () => apiClient.getTikTokFollowings(),
	searchTikTok: (keyword: string) => apiClient.searchTikTok(keyword),
	getTikTokSources: () => apiClient.getTikTokSources(),
	createTikTokSource: (request: CreateYouTubeSourceRequest) =>
		apiClient.createTikTokSource(request),
	setTikTokSourceEnabled: (id: number, enabled: boolean) =>
		apiClient.setTikTokSourceEnabled(id, enabled),
	updateTikTokSource: (id: number, request: UpdateYouTubeSourceRequest) =>
		apiClient.updateTikTokSource(id, request),
	resetTikTokSourcePath: (id: number, new_path: string) =>
		apiClient.resetTikTokSourcePath(id, new_path),
	deleteTikTokSource: (id: number, deleteLocalFiles = false) =>
		apiClient.deleteTikTokSource(id, deleteLocalFiles),
	getDouyinSourceVideos: (params: {
		url: string;
		source_type?: string;
		page?: number;
		page_size?: number;
		keyword?: string;
	}) => apiClient.getDouyinSourceVideos(params),

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
	updateVideoStatus: (id: string | number, request: UpdateVideoStatusRequest) =>
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
	 * 更新视频源本轮扫描已删除视频设置
	 */
	updateVideoSourceScanDeletedOnce: (sourceType: string, id: number, scanDeletedOnce: boolean) =>
		apiClient.updateVideoSourceScanDeletedOnce(sourceType, id, scanDeletedOnce),

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
			split_chapters_after_download?: boolean;
			download_charge_videos?: boolean;
			download_danmaku?: boolean;
			download_subtitle?: boolean;
			download_ai_subtitle?: boolean;
			ai_subtitle_language?: string;
			use_dynamic_api?: boolean;
			ai_rename?: boolean;
			ai_rename_video_prompt?: string;
			ai_rename_audio_prompt?: string;
			ai_rename_enable_multi_page?: boolean;
			ai_rename_enable_collection?: boolean;
			ai_rename_enable_bangumi?: boolean;
			ai_rename_rename_parent_dir?: boolean;
			filter_option?: FilterOption | null;
		}
	) => apiClient.updateVideoSourceDownloadOptions(sourceType, id, options),

	/**
	 * 重设视频源路径
	 */
	resetVideoSourcePath: (sourceType: string, id: number, params: ResetVideoSourcePathRequest) =>
		apiClient.resetVideoSourcePath(sourceType, id, params),

	/**
	 * 重试 UP 投稿源下已识别的充电视频
	 */
	retryChargeVideosForSource: (sourceType: string, id: number) =>
		apiClient.retryChargeVideosForSource(sourceType, id),

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
		caseSensitive: boolean = true,
		minDurationSeconds?: number | null,
		maxDurationSeconds?: number | null,
		publishedAfter?: string,
		publishedBefore?: string
	) =>
		apiClient.updateVideoSourceKeywordFilters(
			sourceType,
			id,
			blacklistKeywords,
			whitelistKeywords,
			caseSensitive,
			minDurationSeconds,
			maxDurationSeconds,
			publishedAfter,
			publishedBefore
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

	testCredentialRefresh: (force = false) => apiClient.testCredentialRefresh(force),

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
	getVideoPlayInfo: (videoId: string | number, options?: { refresh?: boolean }) =>
		apiClient.getVideoPlayInfo(videoId, options),

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
	getLatestIngests: (limit: number = 10, platform: string = 'all') =>
		apiClient.getLatestIngests(limit, platform),

	/**
	 * 获取首页最近处理列表
	 */
	getRecentIngests: (limit: number = 10, platform: string = 'all') =>
		apiClient.getRecentIngests(limit, platform),

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
		webhook_url?: string;
		webhook_bearer_token?: string;
		webhook_custom_headers?: string;
		webhook_format?: string;
		webhook_custom_body?: string;
		webhook_synology_chat_template?: string;
		notification_min_videos?: number;
	}) => apiClient.updateNotificationConfig(config),

	/**
	 * 测试推送通知
	 */
	testNotification: (params?: {
		custom_message?: string;
		active_channel?: string;
		serverchan_key?: string;
		serverchan3_uid?: string;
		serverchan3_sendkey?: string;
		wecom_webhook_url?: string;
		wecom_msgtype?: string;
		wecom_mention_all?: boolean;
		wecom_mentioned_list?: string[];
		webhook_url?: string;
		webhook_bearer_token?: string;
		webhook_custom_headers?: string;
		webhook_format?: string;
		webhook_custom_body?: string;
		webhook_synology_chat_template?: string;
	}) => apiClient.testNotification(params),
	testProxy: (proxy?: string) => apiClient.testProxy(proxy),
	getDatabaseStatus: () => apiClient.getDatabaseStatus(),
	runDatabaseMaintenance: (action: DatabaseMaintenanceAction) =>
		apiClient.runDatabaseMaintenance(action),
	getDatabaseBackups: () => apiClient.getDatabaseBackups(),
	restoreDatabase: (backupFile: string) => apiClient.restoreDatabase(backupFile),
	uploadRestoreBackup: (file: File, filename?: string) => apiClient.uploadRestoreBackup(file, filename),
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
