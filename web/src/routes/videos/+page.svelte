<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import VideoCard from '$lib/components/video-card.svelte';
	import Pagination from '$lib/components/pagination.svelte';
	import SearchBar from '$lib/components/search-bar.svelte';
	import SectionHeader from '$lib/components/section-header.svelte';
	import Loading from '$lib/components/ui/Loading.svelte';
	import SelectAllButton from '$lib/components/select-all-button.svelte';
	import EmptyState from '$lib/components/empty-state.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as AlertDialog from '$lib/components/ui/alert-dialog/index.js';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import FilterIcon from '@lucide/svelte/icons/filter';
	import TrashIcon from '@lucide/svelte/icons/trash-2';
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { setBreadcrumb } from '$lib/stores/breadcrumb';
	import { toast } from 'svelte-sonner';
	import api from '$lib/api';
	import type { VideoInfo } from '$lib/types';
	import type { VideosResponse, VideoSourcesResponse, ApiError } from '$lib/types';
	import { VIDEO_SOURCES, type VideoSourceType } from '$lib/consts';
	import { runRequest } from '$lib/utils/request.js';
	import { buildVideosRequest } from '$lib/utils/videos.js';
	import { buildAuthenticatedStreamUrl } from '$lib/utils/live-stream';
	import { createManagedEventSource } from '$lib/utils/live-event-source';
	import {
		appStateStore,
		resetCurrentPage,
		setAll,
		setCurrentPage,
		setQuery,
		setShowFailedOnly,
		setSort,
		setVideoListInfo,
		ToQuery
	} from '$lib/stores/filter';
	import type { SortBy, SortOrder } from '$lib/types';

	const VIDEOS_PAGE_SIZE_STORAGE_KEY = 'videos.page_size';
	const VIDEOS_GRID_COLS_STORAGE_KEY = 'videos.grid_cols';

	let pageSize = 40;
	let gridCols = 5;
	let pageSizeDraft = String(pageSize);
	let gridColsDraft = String(gridCols);
	let displayPrefsReady = false;

	let videosData: VideosResponse | null = null;
	let videoSources: VideoSourcesResponse | null = null;
	let loading = false;
	let lastSearch: string | null = null;
	const videosStream = createManagedEventSource();
	let liveUpdateStatus: 'idle' | 'connecting' | 'connected' | 'error' = 'idle';
	let pendingInsertedCount = 0;

	// 重置对话框
	let resetAllDialogOpen = false;
	let resettingAll = false;
	let forceReset = false;

	// 重置任务类型选项
	let resetAllTasks = true;
	let resetTaskPages = false;
	let resetTaskVideo = false;
	let resetTaskInfo = false;
	let resetTaskDanmaku = false;
	let resetTaskSubtitle = false;

	// 筛选状态
	let showFilters = false;
	let selectedSourceType: VideoSourceType | '' = '';
	let selectedSourceId = '';
	let showFailedOnly = false;
	let currentSortBy: SortBy = 'id';
	let currentSortOrder: SortOrder = 'desc';

	let selectedResolution = '';
	const RESOLUTION_OPTIONS = [
		{ value: '', label: '全部分辨率' },
		{ value: '2160', label: '2160p' },
		{ value: '1440', label: '1440p' },
		{ value: '1080', label: '1080p' },
		{ value: '720', label: '720p' },
		{ value: '480', label: '480p' },
		{ value: '360', label: '360p' }
	];

	const RESOLUTION_RANGES: Record<string, { min: number; max: number }> = {
		// 说明：B站存在“非标准高度”的情况（例如 1920x1078）。
		// 这里按相邻档位的“中点”划分区间，避免误判（1078 应归为 1080p）。
		'2160': { min: 1800, max: 99999 },
		'1440': { min: 1260, max: 1799 },
		'1080': { min: 900, max: 1259 },
		'720': { min: 600, max: 899 },
		'480': { min: 420, max: 599 },
		'360': { min: 0, max: 419 }
	};

	function getResolutionRange(value: string) {
		if (!value) {
			return null;
		}
		return RESOLUTION_RANGES[value] ?? null;
	}

	function normalizeResolutionRange(minHeight: number | null, maxHeight: number | null) {
		if (minHeight == null && maxHeight == null) {
			return { minHeight: null, maxHeight: null };
		}
		if (minHeight != null && maxHeight != null) {
			return { minHeight, maxHeight };
		}
		if (minHeight != null) {
			const match = Object.values(RESOLUTION_RANGES).find((range) => range.min === minHeight);
			if (match) {
				return { minHeight: match.min, maxHeight: match.max };
			}
		}
		return { minHeight: minHeight ?? null, maxHeight: maxHeight ?? null };
	}

	function getResolutionKey(minHeight: number | null, maxHeight: number | null) {
		if (minHeight == null && maxHeight == null) {
			return '';
		}
		const match = Object.entries(RESOLUTION_RANGES).find(([, range]) => {
			return range.min === minHeight && range.max === maxHeight;
		});
		return match ? match[0] : '';
	}

	function getResolutionLabel(value: string) {
		const option = RESOLUTION_OPTIONS.find((item) => item.value === value);
		return option?.label ?? value;
	}

	function normalizeSortBy(value: string | null): SortBy {
		if (!value) return 'id';
		// 移除 UP主 排序后，兼容旧链接/旧书签
		if (value === 'upper_name') return 'id';
		// 兼容旧参数（created_at 等同于添加时间）
		if (value === 'created_at') return 'id';
		if (
			value === 'id' ||
			value === 'pubtime' ||
			value === 'name' ||
			value === 'is_charge_video' ||
			value === 'file_size'
		)
			return value;
		return 'id';
	}

	function normalizeSortOrder(value: string | null): SortOrder {
		if (value === 'asc' || value === 'desc') return value;
		return 'desc';
	}

	function parseSortValue(value: string): { sortBy: SortBy; sortOrder: SortOrder } {
		const lastUnderscoreIndex = value.lastIndexOf('_');
		if (lastUnderscoreIndex === -1) {
			return {
				sortBy: 'id',
				sortOrder: 'desc'
			};
		}

		return {
			sortBy: normalizeSortBy(value.slice(0, lastUnderscoreIndex)),
			sortOrder: normalizeSortOrder(value.slice(lastUnderscoreIndex + 1))
		};
	}

	// 批量选择状态
	let selectionMode = false;
	let selectedVideos: Set<number> = new Set();
	let batchDeleting = false;
	let batchDeleteDialogOpen = false;

	function loadDisplayPrefs() {
		if (typeof localStorage === 'undefined') return;

		const savedPageSize = Number.parseInt(
			localStorage.getItem(VIDEOS_PAGE_SIZE_STORAGE_KEY) || '',
			10
		);
		if (Number.isFinite(savedPageSize) && savedPageSize > 0) {
			pageSize = savedPageSize;
		}

		const savedGridCols = Number.parseInt(
			localStorage.getItem(VIDEOS_GRID_COLS_STORAGE_KEY) || '',
			10
		);
		if (Number.isFinite(savedGridCols) && savedGridCols > 0) {
			gridCols = savedGridCols;
		}

		pageSizeDraft = String(pageSize);
		gridColsDraft = String(gridCols);

		appStateStore.update((state) => ({
			...state,
			pageSize
		}));
	}

	function buildVideosUrl() {
		const query = ToQuery($appStateStore);
		return query ? `/videos?${query}` : '/videos';
	}

	async function applyPageSize(value: number) {
		if (!Number.isFinite(value) || value <= 0) return;

		const nextValue = Math.trunc(value);
		if (nextValue <= 0) return;
		pageSizeDraft = String(nextValue);
		pageSize = nextValue;
		if (typeof localStorage !== 'undefined') {
			localStorage.setItem(VIDEOS_PAGE_SIZE_STORAGE_KEY, String(nextValue));
		}

		appStateStore.update((state) => ({
			...state,
			pageSize: nextValue
		}));

		resetCurrentPage();

		const nextUrl = buildVideosUrl();
		const currentUrl = `${$page.url.pathname}${$page.url.search}`;
		if (currentUrl === nextUrl || currentUrl === `${nextUrl}?`) {
			const {
				query,
				currentPage,
				videoSource,
				showFailedOnly,
				sortBy,
				sortOrder,
				minHeight,
				maxHeight
			} = $appStateStore;
			await loadVideos(
				query,
				currentPage,
				videoSource,
				showFailedOnly,
				sortBy,
				sortOrder,
				minHeight,
				maxHeight
			);
		} else {
			goto(nextUrl);
		}
	}

	function applyGridCols(value: number) {
		if (!Number.isFinite(value) || value <= 0) return;

		const nextValue = Math.trunc(value);
		if (nextValue <= 0) return;
		gridColsDraft = String(nextValue);
		gridCols = nextValue;
		if (typeof localStorage !== 'undefined') {
			localStorage.setItem(VIDEOS_GRID_COLS_STORAGE_KEY, String(nextValue));
		}
	}

	async function commitPageSize() {
		const value = Number.parseInt(pageSizeDraft, 10);
		if (!Number.isFinite(value) || value <= 0) {
			pageSizeDraft = String(pageSize);
			return;
		}
		await applyPageSize(value);
	}

	function commitGridCols() {
		const value = Number.parseInt(gridColsDraft, 10);
		if (!Number.isFinite(value) || value <= 0) {
			gridColsDraft = String(gridCols);
			return;
		}
		applyGridCols(value);
	}

	function getApiParams(searchParams: URLSearchParams) {
		let videoSource: { type: VideoSourceType; id: string } | null = null;
		for (const source of Object.values(VIDEO_SOURCES)) {
			const value = searchParams.get(source.type);
			if (value) {
				videoSource = { type: source.type, id: value };
			}
		}
		const minHeightRaw = searchParams.get('min_height');
		const maxHeightRaw = searchParams.get('max_height');
		const resolutionRaw = searchParams.get('resolution');
		const minHeightParsed = minHeightRaw ? Number.parseInt(minHeightRaw, 10) : Number.NaN;
		const maxHeightParsed = maxHeightRaw ? Number.parseInt(maxHeightRaw, 10) : Number.NaN;
		const minHeight =
			Number.isFinite(minHeightParsed) && minHeightParsed >= 0 ? minHeightParsed : null;
		const maxHeight =
			Number.isFinite(maxHeightParsed) && maxHeightParsed >= 0 ? maxHeightParsed : null;
		const hasMinMax = Number.isFinite(minHeightParsed) || Number.isFinite(maxHeightParsed);
		const fallbackRange = !hasMinMax && resolutionRaw ? getResolutionRange(resolutionRaw) : null;
		const normalized = normalizeResolutionRange(
			hasMinMax ? minHeight : (fallbackRange?.min ?? null),
			hasMinMax ? maxHeight : (fallbackRange?.max ?? null)
		);

		return {
			query: searchParams.get('query') || '',
			videoSource,
			pageNum: parseInt(searchParams.get('page') || '0'),
			showFailedOnly: searchParams.get('show_failed_only') === 'true',
			minHeight: normalized.minHeight,
			maxHeight: normalized.maxHeight,
			sortBy: normalizeSortBy(searchParams.get('sort_by')),
			sortOrder: normalizeSortOrder(searchParams.get('sort_order'))
		};
	}

	function buildVideosStreamUrl(
		query: string,
		pageNum: number = 0,
		filter?: { type: string; id: string } | null,
		showFailedOnly: boolean = false,
		sortBy: SortBy = 'id',
		sortOrder: SortOrder = 'desc',
		minHeight: number | null = null,
		maxHeight: number | null = null
	): string | null {
		const params = buildVideosRequest({
			page: pageNum,
			pageSize,
			query,
			videoSource: filter,
			showFailedOnly,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		}) as Record<string, string | number | boolean | null | undefined>;

		return buildAuthenticatedStreamUrl('/api/videos/live', params);
	}

	function stopVideosStream() {
		videosStream.stop();
		liveUpdateStatus = 'idle';
	}

	function hasSameVideoOrder(current: VideoInfo[], next: VideoInfo[]): boolean {
		if (current.length !== next.length) return false;
		return current.every((video, index) => video.id === next[index]?.id);
	}

	function patchCurrentPageVideos(current: VideosResponse, next: VideosResponse): VideosResponse {
		const nextById = new Map(next.videos.map((video) => [video.id, video]));
		return {
			...next,
			videos: current.videos.map((video) => nextById.get(video.id) ?? video)
		};
	}

	function applyLiveVideosUpdate(data: VideosResponse) {
		if (!videosData) {
			videosData = data;
			setVideoListInfo(
				data.videos.map((video) => video.id),
				data.total_count,
				pageSize
			);
			pendingInsertedCount = 0;
			return;
		}

		if ($appStateStore.currentPage > 0) {
			const current = videosData;
			const totalDelta = Math.max(0, data.total_count - current.total_count);
			const sameOrder = hasSameVideoOrder(current.videos, data.videos);

			if (!sameOrder || totalDelta > 0) {
				videosData = patchCurrentPageVideos(current, data);
				setVideoListInfo(
					current.videos.map((video) => video.id),
					current.total_count,
					pageSize
				);
				if (totalDelta > 0) {
					pendingInsertedCount = Math.max(pendingInsertedCount, totalDelta);
				}
				return;
			}
		}

		videosData = data;
		setVideoListInfo(
			data.videos.map((video) => video.id),
			data.total_count,
			pageSize
		);
		pendingInsertedCount = 0;

		if (selectionMode) {
			const visibleIds = new Set(data.videos.map((video) => video.id));
			const nextSelected = new Set(Array.from(selectedVideos).filter((id) => visibleIds.has(id)));
			if (nextSelected.size !== selectedVideos.size) {
				selectedVideos = nextSelected;
			}
		}
	}

	function startVideosStream(
		query: string,
		pageNum: number = 0,
		filter?: { type: string; id: string } | null,
		showFailedOnly: boolean = false,
		sortBy: SortBy = 'id',
		sortOrder: SortOrder = 'desc',
		minHeight: number | null = null,
		maxHeight: number | null = null
	) {
		const streamUrl = buildVideosStreamUrl(
			query,
			pageNum,
			filter,
			showFailedOnly,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		);

		const started = videosStream.start({
			url: streamUrl,
			handlers: {
				ready: () => {
					liveUpdateStatus = 'connected';
				},
				videos: (event) => {
					try {
						const payload = JSON.parse(event.data) as VideosResponse;
						applyLiveVideosUpdate(payload);
						liveUpdateStatus = 'connected';
					} catch (error) {
						console.error('解析视频实时更新失败:', error);
					}
				}
			},
			onError: () => {
				liveUpdateStatus = 'error';
			},
			onStop: () => {
				liveUpdateStatus = 'idle';
			}
		});

		if (streamUrl && started) {
			liveUpdateStatus = 'connecting';
		}
	}

	async function handlePendingInsertedClick() {
		pendingInsertedCount = 0;
		setCurrentPage(0);
		await goto(`/videos?${ToQuery($appStateStore)}`);
	}

	async function loadVideos(
		query: string,
		pageNum: number = 0,
		filter?: { type: string; id: string } | null,
		showFailedOnly: boolean = false,
		sortBy: SortBy = 'id',
		sortOrder: SortOrder = 'desc',
		minHeight: number | null = null,
		maxHeight: number | null = null
	) {
		const params = buildVideosRequest({
			page: pageNum,
			pageSize,
			query,
			videoSource: filter,
			showFailedOnly,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		});

		const result = await runRequest(() => api.getVideos(params), {
			setLoading: (value) => (loading = value),
			context: '加载视频失败'
		});
		if (!result) return;

		videosData = result.data;
		// 更新视频列表信息，用于详情页导航
		setVideoListInfo(
			result.data.videos.map((v) => v.id),
			result.data.total_count,
			pageSize
		);
		pendingInsertedCount = 0;
		startVideosStream(
			query,
			pageNum,
			filter,
			showFailedOnly,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		);
	}

	async function loadVideoSources() {
		const result = await runRequest(() => api.getVideoSources(), {
			showErrorToast: false,
			onError: (error) => console.error('加载视频源失败:', error)
		});
		if (!result) return;
		videoSources = result.data;
	}

	async function handlePageChange(pageNum: number) {
		setCurrentPage(pageNum);
		goto(`/videos?${ToQuery($appStateStore)}`);
	}

	async function handleSearchParamsChange(searchParams: URLSearchParams) {
		const {
			query,
			videoSource,
			pageNum,
			showFailedOnly: showFailedOnlyParam,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		} = getApiParams(searchParams);
		setAll(
			query,
			pageNum,
			videoSource,
			showFailedOnlyParam,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		);

		// 同步筛选状态
		if (videoSource) {
			selectedSourceType = videoSource.type;
			selectedSourceId = videoSource.id;
		} else {
			selectedSourceType = '';
			selectedSourceId = '';
		}
		showFailedOnly = showFailedOnlyParam;
		currentSortBy = sortBy;
		currentSortOrder = sortOrder;
		selectedResolution = getResolutionKey(minHeight, maxHeight);

		loadVideos(
			query,
			pageNum,
			videoSource,
			showFailedOnlyParam,
			sortBy,
			sortOrder,
			minHeight,
			maxHeight
		);
	}

	async function handleResetVideo(video: VideoInfo, forceReset: boolean) {
		try {
			const result = await api.resetVideo(video.id, forceReset);
			const data = result.data;
			if (data.resetted) {
				toast.success('重置成功', {
					description: `视频「${video.name}」已重置`
				});
				const {
					query,
					currentPage,
					videoSource,
					showFailedOnly,
					sortBy,
					sortOrder,
					minHeight,
					maxHeight
				} = $appStateStore;
				await loadVideos(
					query,
					currentPage,
					videoSource,
					showFailedOnly,
					sortBy,
					sortOrder,
					minHeight,
					maxHeight
				);
			} else {
				toast.info('重置无效', {
					description: `视频「${video.name}」没有失败的状态，无需重置`
				});
			}
		} catch (error) {
			console.error('重置失败:', error);
			toast.error('重置失败', {
				description: (error as ApiError).message
			});
		}
	}

	async function handleResetAllVideos() {
		resettingAll = true;
		try {
			let result;
			const {
				videoSource,
				query: queryWord,
				showFailedOnly,
				minHeight,
				maxHeight
			} = $appStateStore;

			// 让“批量重置”遵循当前筛选（视频源 / 搜索关键词 / 失败筛选 / 分辨率筛选）
			const filterParams = buildVideosRequest({
				page: 0,
				pageSize: 1,
				query: queryWord,
				videoSource,
				showFailedOnly,
				minHeight,
				maxHeight,
				sortBy: currentSortBy,
				sortOrder: currentSortOrder
			});
			delete filterParams.page;
			delete filterParams.page_size;
			delete filterParams.sort_by;
			delete filterParams.sort_order;

			const finalFilterParams = Object.keys(filterParams).length ? filterParams : undefined;

			if (resetAllTasks) {
				// 重置所有任务，根据当前过滤器传递参数
				result = await api.resetAllVideos(finalFilterParams, forceReset);
			} else {
				// 选择性重置特定任务
				const taskIndexes = [];

				// 根据选择的选项确定要重置的任务索引
				// 注意：一个task_index会同时影响VideoStatus和PageStatus的相同索引
				//
				// 后端状态定义：
				// VideoStatus: [视频封面(0), 视频信息(1), Up主头像(2), Up主信息(3), 分P下载(4)]
				// PageStatus: [视频封面(0), 视频内容(1), 视频信息(2), 视频弹幕(3), 视频字幕(4)]
				//
				// 最终修复的索引映射关系：
				// index 0: Video封面 + Page封面 → 封面图片文件
				// index 1: Video信息(普通视频) + Page内容 → 视频文件(.mp4)，番剧无NFO副作用
				// index 2: Video信息(番剧tvshow.nfo) + Page信息 → tvshow.nfo + 单集NFO文件
				// index 3: Video Up主信息 + Page弹幕 → Up主信息 + 弹幕文件(.ass)
				// index 4: Video 分P下载 + Page字幕 → 分P下载 + 字幕文件

				if (resetTaskPages) taskIndexes.push(0); // 重置封面文件
				if (resetTaskVideo) taskIndexes.push(1); // 重置视频内容 (纯视频文件，番剧无NFO)
				if (resetTaskInfo) taskIndexes.push(2); // 重置视频信息 (tvshow.nfo + 单集NFO)
				if (resetTaskDanmaku) taskIndexes.push(3); // 重置弹幕文件 (弹幕 + Up主信息)
				if (resetTaskSubtitle) taskIndexes.push(4); // 重置字幕文件 (字幕 + 分P下载)

				// 去重任务索引
				const uniqueTaskIndexes = [...new Set(taskIndexes)];

				if (uniqueTaskIndexes.length === 0) {
					toast.error('请至少选择一个要重置的任务');
					return;
				}

				// 调用选择性重置API，根据当前过滤器传递参数
				result = await api.resetSpecificTasks(uniqueTaskIndexes, finalFilterParams, forceReset);
			}

			const data = result.data;
			if (data.resetted) {
				toast.success('重置成功', {
					description: `已重置 ${data.resetted_videos_count} 个视频和 ${data.resetted_pages_count} 个分页`
				});
				// 延迟重新加载视频列表，避免与toast提示冲突
				setTimeout(async () => {
					const {
						query,
						currentPage,
						videoSource: currentVideoSource,
						showFailedOnly,
						sortBy,
						sortOrder,
						minHeight,
						maxHeight
					} = $appStateStore;
					await loadVideos(
						query,
						currentPage,
						currentVideoSource,
						showFailedOnly,
						sortBy,
						sortOrder,
						minHeight,
						maxHeight
					);
				}, 100);
			} else {
				toast.info('没有需要重置的视频');
			}
		} catch (error) {
			console.error('重置失败:', error);
			toast.error('重置失败', {
				description: (error as ApiError).message
			});
		} finally {
			resettingAll = false;
			resetAllDialogOpen = false;
		}
	}

	function handleSourceFilter(sourceType: VideoSourceType, sourceId: string) {
		selectedSourceType = sourceType;
		selectedSourceId = sourceId;
		const range = getResolutionRange(selectedResolution);
		setAll(
			'',
			0,
			{ type: sourceType, id: sourceId },
			showFailedOnly,
			currentSortBy,
			currentSortOrder,
			range ? range.min : null,
			range ? range.max : null
		);
		goto(`/videos?${ToQuery($appStateStore)}`);
	}

	function clearFilters() {
		selectedSourceType = '';
		selectedSourceId = '';
		showFailedOnly = false;
		selectedResolution = '';
		currentSortBy = 'id';
		currentSortOrder = 'desc';
		setAll('', 0, null, false, 'id', 'desc', null, null);
		goto('/videos');
	}

	function handleSortChange(sortBy: SortBy, sortOrder: SortOrder) {
		currentSortBy = sortBy;
		currentSortOrder = sortOrder;
		setSort(sortBy, sortOrder);
		resetCurrentPage();
		goto(`/videos?${ToQuery($appStateStore)}`);
	}

	function handleResolutionChange(value: string) {
		selectedResolution = value;
		const range = getResolutionRange(value);
		const nextMinHeight = range ? range.min : null;
		const nextMaxHeight = range ? range.max : null;
		const { query, videoSource } = $appStateStore;
		setAll(
			query,
			0,
			videoSource,
			showFailedOnly,
			currentSortBy,
			currentSortOrder,
			nextMinHeight,
			nextMaxHeight
		);
		const nextState = {
			...$appStateStore,
			query,
			currentPage: 0,
			videoSource,
			showFailedOnly,
			sortBy: currentSortBy,
			sortOrder: currentSortOrder,
			minHeight: nextMinHeight,
			maxHeight: nextMaxHeight
		};
		const nextQuery = ToQuery(nextState);
		goto(nextQuery ? `/videos?${nextQuery}` : '/videos');
	}

	// 处理重置任务选择
	function handleResetAllTasksChange() {
		if (resetAllTasks) {
			resetTaskPages = false;
			resetTaskVideo = false;
			resetTaskInfo = false;
			resetTaskDanmaku = false;
			resetTaskSubtitle = false;
		}
	}

	function handleSpecificTaskChange() {
		if (
			resetTaskPages ||
			resetTaskVideo ||
			resetTaskInfo ||
			resetTaskDanmaku ||
			resetTaskSubtitle
		) {
			resetAllTasks = false;
		}
	}

	// 批量选择相关函数
	function toggleSelectionMode() {
		selectionMode = !selectionMode;
		selectedVideos.clear();
		selectedVideos = selectedVideos; // 触发反应式更新
	}

	function handleVideoSelection(videoId: number, selected: boolean) {
		if (selected) {
			selectedVideos.add(videoId);
		} else {
			selectedVideos.delete(videoId);
		}
		selectedVideos = selectedVideos; // 触发反应式更新
	}

	function selectAllVideos() {
		if (videosData?.videos) {
			videosData.videos.forEach((video) => selectedVideos.add(video.id));
			selectedVideos = selectedVideos;
		}
	}

	function clearSelection() {
		selectedVideos.clear();
		selectedVideos = selectedVideos;
	}

	function isQueuedDeleteMessage(message?: string | null): boolean {
		if (!message) return false;
		return message.includes('加入队列');
	}

	async function handleBatchDelete() {
		if (selectedVideos.size === 0) return;

		batchDeleting = true;
		let successCount = 0;
		let queuedCount = 0;
		let failedCount = 0;
		const selectedVideoIds = Array.from(selectedVideos);

		try {
			for (let i = 0; i < selectedVideoIds.length; i++) {
				const videoId = selectedVideoIds[i];
				try {
					const result = await api.deleteVideo(videoId);
					if (result.data.success) {
						if (isQueuedDeleteMessage(result.data.message)) {
							queuedCount++;
						} else {
							successCount++;
						}
					} else {
						failedCount++;
					}
				} catch (error) {
					failedCount++;
					console.error(`删除视频 ${videoId} 失败:`, error);
				}
			}

			if (successCount > 0 || queuedCount > 0) {
				if (successCount > 0) {
					toast.success('批量删除完成', {
						description: `成功删除 ${successCount} 个视频${queuedCount > 0 ? `，已入队 ${queuedCount} 个` : ''}${failedCount > 0 ? `，失败 ${failedCount} 个` : ''}`
					});
				} else {
					toast.info('批量删除任务已入队', {
						description: `已加入队列 ${queuedCount} 个视频${failedCount > 0 ? `，失败 ${failedCount} 个` : ''}，将在扫描完成后自动处理`
					});
				}

				// 重新加载视频列表
				const {
					query,
					currentPage,
					videoSource,
					showFailedOnly,
					sortBy,
					sortOrder,
					minHeight,
					maxHeight
				} = $appStateStore;
				await loadVideos(
					query,
					currentPage,
					videoSource,
					showFailedOnly,
					sortBy,
					sortOrder,
					minHeight,
					maxHeight
				);

				// 清空选择
				clearSelection();
			} else {
				toast.error('批量删除失败', {
					description: '所有视频都删除失败'
				});
			}
		} catch (error) {
			console.error('批量删除过程中发生错误:', error);
			toast.error('批量删除失败', {
				description: '删除过程中发生错误'
			});
		} finally {
			batchDeleting = false;
			batchDeleteDialogOpen = false;
		}
	}

	$: if (displayPrefsReady && $page.url.search !== lastSearch) {
		lastSearch = $page.url.search;
		handleSearchParamsChange($page.url.searchParams);
	}

	$: totalPages = videosData ? Math.ceil(videosData.total_count / pageSize) : 0;

	onMount(() => {
		setBreadcrumb([{ label: '视频管理' }]);
		loadDisplayPrefs();
		displayPrefsReady = true;
		loadVideoSources();
	});

	onDestroy(() => {
		stopVideosStream();
	});
</script>

<svelte:head>
	<title>视频管理 - Bili Sync</title>
</svelte:head>

<div class="space-y-6">
	<SectionHeader
		as="h1"
		title="视频管理"
		description="搜索、筛选并批量管理已同步的视频列表。"
		titleClass="text-2xl font-bold"
		descriptionClass="text-muted-foreground mt-1 text-sm"
	/>

	<!-- 搜索和筛选栏 -->
	<div class="flex flex-col gap-4">
		<!-- 搜索栏 -->
		<div class="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
			<div class="w-full sm:max-w-md sm:flex-1">
				<SearchBar
					placeholder="搜索视频标题..."
					value={$appStateStore.query}
					onSearch={(value) => {
						setQuery(value);
						resetCurrentPage();
						goto(`/videos?${ToQuery($appStateStore)}`);
					}}
				/>
			</div>

			<div
				class="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:flex-wrap sm:items-center sm:justify-end lg:flex-nowrap"
			>
				<!-- 排序 + 显示数量 -->
				<div class="flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:items-center">
					<!-- 排序下拉框 - 在移动端占满宽度 -->
					<div class="w-full sm:w-auto">
						<select
							class="border-input bg-background ring-offset-background focus:ring-ring h-9 w-full rounded-md border px-3 py-1 text-sm focus:ring-2 focus:ring-offset-2 focus:outline-none sm:w-auto"
							value="{currentSortBy}_{currentSortOrder}"
							onchange={(e) => {
								const { sortBy, sortOrder } = parseSortValue(e.currentTarget.value);
								handleSortChange(sortBy, sortOrder);
							}}
						>
							<option value="id_desc">添加时间 (最新)</option>
							<option value="id_asc">添加时间 (最早)</option>
							<option value="pubtime_desc">发布时间 (最新)</option>
							<option value="pubtime_asc">发布时间 (最早)</option>
							<option value="is_charge_video_desc">充电视频在前</option>
							<option value="file_size_desc">文件大小 (最大)</option>
							<option value="file_size_asc">文件大小 (最小)</option>
							<option value="name_asc">名称 (A-Z)</option>
							<option value="name_desc">名称 (Z-A)</option>
						</select>
					</div>

					<!-- 显示数量设置 -->
					<div class="grid grid-cols-2 gap-2 sm:flex sm:items-center sm:gap-2">
						<label class="flex items-center gap-2">
							<span class="text-muted-foreground text-sm whitespace-nowrap">每页</span>
							<input
								class="border-input bg-background ring-offset-background focus:ring-ring h-9 w-full min-w-0 rounded-md border px-2 py-1 text-sm focus:ring-2 focus:ring-offset-2 focus:outline-none sm:w-24"
								type="number"
								min="1"
								step="1"
								bind:value={pageSizeDraft}
								onchange={commitPageSize}
							/>
						</label>
						<label class="flex items-center gap-2">
							<span class="text-muted-foreground text-sm whitespace-nowrap">每行</span>
							<input
								class="border-input bg-background ring-offset-background focus:ring-ring h-9 w-full min-w-0 rounded-md border px-2 py-1 text-sm focus:ring-2 focus:ring-offset-2 focus:outline-none sm:w-24"
								type="number"
								min="1"
								step="1"
								bind:value={gridColsDraft}
								onchange={commitGridCols}
							/>
						</label>
					</div>
				</div>

				<!-- 操作按钮栏 - 移动端使用网格布局 -->
				<div class="grid grid-cols-2 gap-2 sm:flex sm:items-center sm:gap-2">
					<!-- 筛选按钮 -->
					<Button
						variant={showFilters ? 'default' : 'outline'}
						size="sm"
						class="w-full sm:w-auto"
						onclick={() => (showFilters = !showFilters)}
					>
						<FilterIcon class="mr-2 h-4 w-4" />
						<span class="xs:inline hidden">筛选</span>
						<span class="xs:hidden">筛选</span>
					</Button>

					<!-- 显示错误视频按钮 -->
					<Button
						variant={showFailedOnly ? 'destructive' : 'outline'}
						size="sm"
						class="w-full sm:w-auto"
						onclick={() => {
							showFailedOnly = !showFailedOnly;
							setShowFailedOnly(showFailedOnly);
							resetCurrentPage();
							goto(`/videos?${ToQuery($appStateStore)}`);
						}}
					>
						<span class="hidden sm:inline">只显示错误视频</span>
						<span class="sm:hidden">错误视频</span>
					</Button>

					<!-- 批量重置按钮 -->
					<Button
						variant="outline"
						size="sm"
						class="col-span-2 w-full sm:col-span-1 sm:w-auto"
						onclick={() => (resetAllDialogOpen = true)}
						disabled={resettingAll || loading}
						title="按当前筛选条件批量重置视频任务状态"
					>
						<RotateCcwIcon class="mr-2 h-4 w-4 {resettingAll ? 'animate-spin' : ''}" />
						<span class="xs:inline hidden">批量重置</span>
						<span class="xs:hidden">重置</span>
					</Button>

					<!-- 批量删除模式按钮 -->
					<Button
						variant={selectionMode ? 'outline' : 'destructive'}
						size="sm"
						class="col-span-2 w-full sm:col-span-1 sm:w-auto {selectionMode
							? 'border-blue-600 bg-blue-600 text-white hover:bg-blue-700 dark:bg-blue-500 dark:hover:bg-blue-600'
							: ''}"
						onclick={toggleSelectionMode}
						disabled={loading}
					>
						{#if selectionMode}
							<span>退出</span>
						{:else}
							<TrashIcon class="h-4 w-4 sm:mr-2" />
							<span class="hidden sm:inline">批量删除</span>
						{/if}
					</Button>
				</div>
			</div>
		</div>
	</div>

	<!-- 批量操作工具栏 -->
	{#if selectionMode}
		<div
			class="space-y-3 rounded-lg border border-blue-200 bg-blue-50/50 p-3 dark:border-blue-800 dark:bg-blue-950/20"
		>
			<div class="flex items-center justify-between gap-2">
				<div class="text-sm font-medium text-blue-700 dark:text-blue-300">
					已选择 {selectedVideos.size} 个视频--将进行批量删除！！！
				</div>
				<div class="flex gap-2">
					{#if videosData?.videos && selectedVideos.size < videosData.videos.length}
						<SelectAllButton onclick={selectAllVideos} className="text-sm" />
					{/if}
					{#if selectedVideos.size > 0}
						<Button variant="outline" size="sm" onclick={clearSelection}>取消选中</Button>
						<Button
							variant="destructive"
							size="sm"
							onclick={() => (batchDeleteDialogOpen = true)}
							disabled={batchDeleting}
						>
							删除选中
						</Button>
					{/if}
				</div>
			</div>
		</div>
	{/if}

	<!-- 筛选面板 -->
	{#if showFilters && videoSources}
		<div class="space-y-3 rounded-lg border p-3">
			<div class="flex items-center justify-between">
				<h3 class="text-sm font-medium">按视频源筛选</h3>
				{#if selectedSourceType}
					<Button variant="ghost" size="sm" onclick={clearFilters}>清除筛选</Button>
				{/if}
			</div>

			<div class="space-y-3">
				{#each Object.entries(VIDEO_SOURCES) as [sourceKey, sourceConfig] (sourceKey)}
					{@const sources = videoSources[sourceConfig.type as VideoSourceType]}
					{#if sources && sources.length > 0}
						<div class="space-y-2">
							<div class="flex items-center gap-2">
								<sourceConfig.icon class="text-muted-foreground h-4 w-4" />
								<span class="text-sm font-medium">{sourceConfig.title}</span>
								<Badge variant="outline" class="text-xs">{sources.length}</Badge>
							</div>
							<div class="flex flex-wrap gap-1">
								{#each sources as source (source.id)}
									<Button
										variant={selectedSourceType === sourceConfig.type &&
										selectedSourceId === source.id.toString()
											? 'default'
											: 'outline'}
										size="sm"
										class="h-7 text-xs {!source.enabled ? 'opacity-60' : ''}"
										onclick={() => handleSourceFilter(sourceConfig.type, source.id.toString())}
									>
										{source.name}
										{#if !source.enabled}
											<span class="ml-1 text-xs opacity-70">(禁用)</span>
										{/if}
									</Button>
								{/each}
							</div>
						</div>
					{/if}
				{/each}
			</div>

			<div class="border-t pt-3">
				<div class="flex items-center gap-2">
					<span class="text-sm font-medium">按分辨率筛选</span>
				</div>
				<div class="mt-2 w-full sm:max-w-xs">
					<select
						class="border-input bg-background ring-offset-background focus:ring-ring h-9 w-full rounded-md border px-3 py-1 text-sm focus:ring-2 focus:ring-offset-2 focus:outline-none"
						bind:value={selectedResolution}
						onchange={(e) => handleResolutionChange(e.currentTarget.value)}
					>
						{#each RESOLUTION_OPTIONS as option}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
				</div>
			</div>
		</div>
	{/if}

	<!-- 当前筛选状态 -->
	{#if (selectedSourceType && selectedSourceId && videoSources) || showFailedOnly || selectedResolution}
		<div class="flex flex-wrap items-center gap-2">
			<span class="text-muted-foreground text-sm">当前筛选:</span>

			{#if selectedSourceType && selectedSourceId && videoSources}
				{@const sourceConfig = Object.values(VIDEO_SOURCES).find(
					(config) => config.type === selectedSourceType
				)}
				{@const sources = videoSources[selectedSourceType as VideoSourceType]}
				{@const currentSource = sources?.find((s) => s.id.toString() === selectedSourceId)}
				{#if sourceConfig && currentSource}
					<Badge variant="secondary" class="flex items-center gap-1">
						<sourceConfig.icon class="h-3 w-3" />
						{currentSource.name}
						<button onclick={clearFilters} class="hover:bg-muted-foreground/20 ml-1 rounded">
							<span class="sr-only">清除筛选</span>
							×
						</button>
					</Badge>
				{/if}
			{/if}

			{#if selectedResolution}
				<Badge variant="secondary" class="flex items-center gap-1"
					>分辨率 {getResolutionLabel(selectedResolution)}
					<button
						onclick={() => handleResolutionChange('')}
						class="hover:bg-muted-foreground/20 ml-1 rounded"
					>
						<span class="sr-only">清除分辨率筛选</span>
						x
					</button>
				</Badge>
			{/if}

			{#if showFailedOnly}
				<Badge variant="destructive" class="flex items-center gap-1">
					只显示错误视频
					<button
						onclick={() => {
							showFailedOnly = false;
							setShowFailedOnly(false);
							resetCurrentPage();
							goto(`/videos?${ToQuery($appStateStore)}`);
						}}
						class="hover:bg-muted-foreground/20 ml-1 rounded"
					>
						<span class="sr-only">清除错误视频筛选</span>
						×
					</button>
				</Badge>
			{/if}

			{#if (selectedSourceType && selectedSourceId) || showFailedOnly || selectedResolution}
				<Button variant="ghost" size="sm" onclick={clearFilters}>清除所有筛选</Button>
			{/if}
		</div>
	{/if}

	<!-- 视频列表统计 -->
	{#if videosData}
		<div class="text-muted-foreground flex items-center justify-between text-sm">
			<span title="显示当前筛选结果总数和所在分页">
				共 {videosData.total_count} 个视频，当前第 {$appStateStore.currentPage + 1} / {totalPages} 页
			</span>
			<Badge
				variant="outline"
				class={liveUpdateStatus === 'connected'
					? 'border-green-200 bg-green-50 text-green-700'
					: liveUpdateStatus === 'connecting'
						? 'border-blue-200 bg-blue-50 text-blue-700'
						: liveUpdateStatus === 'error'
							? 'border-yellow-200 bg-yellow-50 text-yellow-700'
							: 'border-muted-foreground/20 text-muted-foreground'}
				title={liveUpdateStatus === 'connected'
					? '视频下载和任务状态会通过实时连接自动更新'
					: liveUpdateStatus === 'connecting'
						? '正在建立实时连接，建立后会自动更新进度'
						: liveUpdateStatus === 'error'
							? '实时连接正在重试，恢复后会继续自动更新进度'
							: '当前未建立实时连接，需要手动刷新查看最新进度'}
			>
				{#if liveUpdateStatus === 'connected'}
					进度实时更新中
				{:else if liveUpdateStatus === 'connecting'}
					进度实时连接中
				{:else if liveUpdateStatus === 'error'}
					进度实时重连中
				{:else}
					进度实时未开启
				{/if}
			</Badge>
		</div>
		{#if currentSortBy === 'file_size' && videosData.file_size_stats_pending}
			<div class="text-muted-foreground mt-2 text-sm">
				文件大小统计中，首次按文件大小排序时会在后台逐步补全旧文件大小。
			</div>
		{/if}
		{#if $appStateStore.currentPage > 0 && pendingInsertedCount > 0}
			<div class="mt-2">
				<Button
					variant="outline"
					size="sm"
					onclick={handlePendingInsertedClick}
					title="跳转到第一页查看新入库视频"
				>
					当前页外有 {pendingInsertedCount} 个新入库视频，点击跳到第一页查看
				</Button>
			</div>
		{/if}
	{/if}

	<!-- 视频卡片网格 -->
	{#if loading}
		<Loading size="lg" />
	{:else if videosData?.videos.length}
		<div
			class="grid gap-4 sm:grid-cols-2 md:grid-cols-[repeat(var(--videos-grid-cols),minmax(0,1fr))]"
			style={`--videos-grid-cols: ${gridCols};`}
		>
			{#each videosData.videos as video (video.id)}
				<VideoCard
					{video}
					{selectionMode}
					selected={selectedVideos.has(video.id)}
					onSelectionChange={handleVideoSelection}
					onReset={async (forceReset) => {
						await handleResetVideo(video, forceReset);
					}}
				/>
			{/each}
		</div>

		<!-- 分页 -->
		{#if totalPages > 1}
			<Pagination
				currentPage={$appStateStore.currentPage}
				{totalPages}
				onPageChange={handlePageChange}
			/>
		{/if}
	{:else}
		<EmptyState title="暂无视频数据" description="尝试调整搜索条件或添加视频源" class="py-16" />
	{/if}
</div>

<!-- 批量重置确认对话框 -->
<AlertDialog.Root bind:open={resetAllDialogOpen}>
	<AlertDialog.Content>
		<AlertDialog.Header>
			<AlertDialog.Title>批量重置确认</AlertDialog.Title>
			<AlertDialog.Description>
				{#if selectedSourceType && selectedSourceId && videoSources}
					{@const sourceConfig = Object.values(VIDEO_SOURCES).find(
						(config) => config.type === selectedSourceType
					)}
					{@const sources = videoSources[selectedSourceType as VideoSourceType]}
					{@const currentSource = sources?.find((s) => s.id.toString() === selectedSourceId)}
					{#if sourceConfig && currentSource}
						确定要重置「{currentSource.name}」视频源下的所有视频状态吗？此操作将清除失败状态并重新开始下载。
					{:else}
						确定要重置当前筛选条件下的所有视频状态吗？此操作将清除失败状态并重新开始下载。
					{/if}
				{:else}
					确定要重置所有视频状态吗？此操作将清除失败状态并重新开始下载。
				{/if}
			</AlertDialog.Description>
		</AlertDialog.Header>
		<div class="space-y-4 py-4">
			<!-- 重置模式选择 -->
			<div class="space-y-2">
				<div class="text-sm font-medium">重置模式：</div>
				<div class="space-y-2 rounded-lg border p-3">
					<label class="flex items-center gap-2">
						<input type="radio" bind:group={forceReset} value={false} />
						<span class="text-sm">只重置失败的任务（推荐）</span>
					</label>
					<label class="flex items-center gap-2">
						<input type="radio" bind:group={forceReset} value={true} />
						<span class="text-sm">强制重置所有任务（包括已完成的）</span>
					</label>
				</div>
			</div>

			<!-- 任务类型选择 -->
			<div class="space-y-3">
				<div class="text-sm font-medium">选择要重置的任务类型：</div>

				<!-- 重置所有任务 -->
				<label class="flex items-center gap-2">
					<input
						type="checkbox"
						bind:checked={resetAllTasks}
						onchange={handleResetAllTasksChange}
						class="rounded border-gray-300"
					/>
					<span class="text-sm font-medium">重置所有任务类型</span>
				</label>

				<!-- 或选择特定任务 -->
				<div class="ml-4 space-y-2">
					<div class="text-muted-foreground text-sm">或选择特定任务：</div>

					<label class="flex items-center gap-2">
						<input
							type="checkbox"
							bind:checked={resetTaskPages}
							onchange={handleSpecificTaskChange}
							disabled={resetAllTasks}
							class="rounded border-gray-300"
						/>
						<span class="text-sm">重置视频封面</span>
					</label>

					<label class="flex items-center gap-2">
						<input
							type="checkbox"
							bind:checked={resetTaskVideo}
							onchange={handleSpecificTaskChange}
							disabled={resetAllTasks}
							class="rounded border-gray-300"
						/>
						<span class="text-sm">重置视频内容</span>
					</label>

					<label class="flex items-center gap-2">
						<input
							type="checkbox"
							bind:checked={resetTaskInfo}
							onchange={handleSpecificTaskChange}
							disabled={resetAllTasks}
							class="rounded border-gray-300"
						/>
						<span class="text-sm">重置视频信息</span>
					</label>

					<label class="flex items-center gap-2">
						<input
							type="checkbox"
							bind:checked={resetTaskDanmaku}
							onchange={handleSpecificTaskChange}
							disabled={resetAllTasks}
							class="rounded border-gray-300"
						/>
						<span class="text-sm">重置视频弹幕</span>
					</label>

					<label class="flex items-center gap-2">
						<input
							type="checkbox"
							bind:checked={resetTaskSubtitle}
							onchange={handleSpecificTaskChange}
							disabled={resetAllTasks}
							class="rounded border-gray-300"
						/>
						<span class="text-sm">重置视频字幕</span>
					</label>
				</div>

				<!-- 注意事项 -->
				<div class="mt-4 rounded-lg border border-yellow-200 bg-yellow-50 p-3">
					<div class="text-sm text-yellow-800">
						<strong>说明：</strong>
						<ul class="mt-1 list-inside list-disc">
							<li>"只重置失败的任务"模式只会重置状态为失败的任务</li>
							<li>"强制重置"模式会将所有选中的任务重置为"未开始"状态</li>
							<li>选择特定任务类型时，会同时重置对应的分P下载状态</li>
						</ul>
					</div>
				</div>
			</div>
		</div>
		<AlertDialog.Footer>
			<AlertDialog.Cancel disabled={resettingAll}>取消</AlertDialog.Cancel>
			<AlertDialog.Action onclick={handleResetAllVideos} disabled={resettingAll}>
				{resettingAll ? '重置中...' : '确认重置'}
			</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>

<!-- 批量删除确认对话框 -->
<AlertDialog.Root bind:open={batchDeleteDialogOpen}>
	<AlertDialog.Content>
		<AlertDialog.Header>
			<AlertDialog.Title>确认批量删除视频</AlertDialog.Title>
			<AlertDialog.Description>
				确定要删除选中的 <span class="font-medium text-red-600">{selectedVideos.size}</span> 个视频吗？
			</AlertDialog.Description>
		</AlertDialog.Header>
		<div class="py-4">
			<div
				class="rounded-lg border border-yellow-200 bg-yellow-50 p-3 dark:border-yellow-800 dark:bg-yellow-950/20"
			>
				<div class="text-sm text-yellow-800 dark:text-yellow-200">
					<strong>注意：</strong>
					<ul class="mt-1 list-inside list-disc">
						<li>此操作不可撤销</li>
						<li>删除当前视频后，在视频源设置中开启"扫描已删除视频"后可重新下载</li>
						<li>视频文件和相关元数据将被标记为已删除</li>
					</ul>
				</div>
			</div>
		</div>
		<AlertDialog.Footer>
			<AlertDialog.Cancel disabled={batchDeleting}>取消</AlertDialog.Cancel>
			<AlertDialog.Action
				onclick={handleBatchDelete}
				disabled={batchDeleting}
				class="bg-red-600 hover:bg-red-700 focus:ring-red-600"
			>
				{batchDeleting ? '删除中...' : '确认删除'}
			</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
