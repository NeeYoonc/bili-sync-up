import { writable } from 'svelte/store';
import type { SortBy, SortOrder } from '$lib/types';

export interface AppState {
	platform: 'bilibili' | 'youtube' | 'douyin' | 'tiktok';
	query: string;
	currentPage: number;
	videoSource: {
		type: string;
		id: string;
	} | null;
	showFailedOnly: boolean;
	minHeight: number | null;
	maxHeight: number | null;
	sortBy: SortBy;
	sortOrder: SortOrder;
	videoIds: number[]; // 当前视频列表的 ID，用于详情页导航
	totalCount: number; // 视频总数
	pageSize: number; // 每页大小
}

export const appStateStore = writable<AppState>({
	platform: 'bilibili',
	query: '',
	currentPage: 0,
	videoSource: null,
	showFailedOnly: false,
	minHeight: null,
	maxHeight: null,
	sortBy: 'id',
	sortOrder: 'desc',
	videoIds: [],
	totalCount: 0,
	pageSize: 40
});

export const ToQuery = (state: AppState): string => {
	const { query, videoSource, showFailedOnly, sortBy, sortOrder, minHeight, maxHeight } = state;
	const params = new URLSearchParams();
	if (state.platform !== 'bilibili') {
		params.set('platform', state.platform);
	}
	if (state.currentPage > 0) {
		params.set('page', String(state.currentPage));
	}
	if (query.trim()) {
		params.set('query', query);
	}
	if (videoSource && videoSource.type && videoSource.id) {
		params.set(videoSource.type, videoSource.id);
	}
	if (showFailedOnly) {
		params.set('show_failed_only', 'true');
	}
	if (typeof minHeight === 'number' && Number.isFinite(minHeight)) {
		params.set('min_height', String(minHeight));
	}
	if (typeof maxHeight === 'number' && Number.isFinite(maxHeight)) {
		params.set('max_height', String(maxHeight));
	}
	// 只有非默认排序时才添加到URL
	if (sortBy !== 'id' || sortOrder !== 'desc') {
		params.set('sort_by', sortBy);
		params.set('sort_order', sortOrder);
	}
	const queryString = params.toString();
	return queryString ? `${queryString}` : '';
};

export const setQuery = (query: string) => {
	appStateStore.update((state) => ({
		...state,
		query
	}));
};

export const setVideoSourceFilter = (type: string, id: string) => {
	appStateStore.update((state) => ({
		...state,
		videoSource: { type, id }
	}));
};

export const clearVideoSourceFilter = () => {
	appStateStore.update((state) => ({
		...state,
		videoSource: null
	}));
};

export const setCurrentPage = (page: number) => {
	appStateStore.update((state) => ({
		...state,
		currentPage: page
	}));
};

export const resetCurrentPage = () => {
	appStateStore.update((state) => ({
		...state,
		currentPage: 0
	}));
};

export const setShowFailedOnly = (showFailedOnly: boolean) => {
	appStateStore.update((state) => ({
		...state,
		showFailedOnly
	}));
};

export const setAll = (
	query: string,
	currentPage: number,
	videoSource: { type: string; id: string } | null,
	showFailedOnly: boolean = false,
	sortBy: SortBy = 'id',
	sortOrder: SortOrder = 'desc',
	minHeight: number | null = null,
	maxHeight: number | null = null
) => {
	appStateStore.update((state) => ({
		...state,
		query,
		currentPage,
		videoSource,
		showFailedOnly,
		minHeight,
		maxHeight,
		sortBy,
		sortOrder
	}));
};

export const clearAll = () => {
	appStateStore.update((state) => ({
		...state,
		query: '',
		currentPage: 0,
		videoSource: null,
		showFailedOnly: false,
		minHeight: null,
		maxHeight: null,
		sortBy: 'id',
		sortOrder: 'desc'
	}));
};

export const setPlatform = (platform: 'bilibili' | 'youtube' | 'douyin' | 'tiktok') => {
	appStateStore.update((state) => ({
		...state,
		platform
	}));
};

export const setSort = (sortBy: SortBy, sortOrder: SortOrder) => {
	appStateStore.update((state) => ({
		...state,
		sortBy,
		sortOrder
	}));
};

export const setVideoIds = (videoIds: number[]) => {
	appStateStore.update((state) => ({
		...state,
		videoIds
	}));
};

export const setTotalCount = (totalCount: number) => {
	appStateStore.update((state) => ({
		...state,
		totalCount
	}));
};

export const setVideoListInfo = (videoIds: number[], totalCount: number, pageSize: number) => {
	appStateStore.update((state) => ({
		...state,
		videoIds,
		totalCount,
		pageSize
	}));
};

// 保留旧的接口以兼容现有代码
export const filterStore = writable({ key: '', value: '' });
export const setFilter = (key: string, value: string) => setVideoSourceFilter(key, value);
export const clearFilter = clearVideoSourceFilter;
