<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import BiliImage from '$lib/components/bili-image.svelte';
	import { Button } from '$lib/components/ui/button';
	import { X } from '@lucide/svelte';
	import api from '$lib/api';
	import type { SubmissionVideoInfo } from '$lib/types';
	import { toast } from 'svelte-sonner';
	import { formatTimestamp } from '$lib/utils/timezone';

	export let isOpen = false;
	export let sourceId: number;
	export let upperId: number;
	export let upperName: string;
	export let initialSelectedVideos: string[] = [];

	const dispatch = createEventDispatcher<{
		confirm: string[];
		cancel: void;
	}>();

	// 投稿视频列表
	let submissionVideos: SubmissionVideoInfo[] = [];
	let selectedSubmissionVideos: Set<string> = new Set();
	let submissionLoading = false;
	let submissionError: string | null = null;
	let submissionTotalCount = 0;
	let submissionSearchQuery = '';
	let filteredSubmissionVideos: SubmissionVideoInfo[] = [];

	// 已下载视频的BVID列表
	let downloadedBvids: Set<string> = new Set();
	let loadingDownloaded = false;

	// 分页加载相关状态
	let currentLoadedPage = 0;
	let isLoadingMore = false;
	let hasMoreVideos = true;
	let loadingProgress = '';
	let showLoadMoreButton = false;
	let isSearching = false;

	// 滚动容器引用
	let submissionScrollContainer: HTMLElement;

	const SUBMISSION_PAGE_SIZE = 20;
	const INITIAL_LOAD_SIZE = 100;
	const LOAD_MORE_SIZE = 200;
	const PAGE_DELAY = 500;

	// 已选中视频数量
	$: selectedSubmissionCount = selectedSubmissionVideos.size;

	// 格式化时间
	function formatSubmissionDate(pubtime: string): string {
		const formatted = formatTimestamp(pubtime, 'Asia/Shanghai', 'date');
		if (formatted === '无效时间' || formatted === '格式化失败') {
			return pubtime;
		}
		return formatted;
	}

	// 格式化播放量
	function formatSubmissionPlayCount(count: number): string {
		if (count >= 10000) {
			return (count / 10000).toFixed(1) + '万';
		}
		return count.toString();
	}

	// 当对话框打开时加载数据
	$: if (isOpen && upperId) {
		resetAndLoad();
	}

	// 重置状态并加载
	async function resetAndLoad() {
		submissionVideos = [];
		selectedSubmissionVideos = new Set(initialSelectedVideos);
		submissionLoading = false;
		submissionError = null;
		submissionTotalCount = 0;
		submissionSearchQuery = '';
		filteredSubmissionVideos = [];
		currentLoadedPage = 0;
		hasMoreVideos = true;
		showLoadMoreButton = false;
		downloadedBvids = new Set();

		// 并行加载已下载视频和UP主投稿
		await Promise.all([loadDownloadedVideos(), loadSubmissionVideos()]);

		// 两个请求都完成后，再更新过滤列表
		updateFilteredVideos();
	}

	// 加载该投稿源已下载的视频BVID列表
	async function loadDownloadedVideos() {
		loadingDownloaded = true;
		try {
			// 获取该投稿源的所有视频
			const response = await api.getVideos({
				submission: sourceId,
				page: 0,
				page_size: 10000 // 获取全部
			});

			if (response.data && response.data.videos) {
				downloadedBvids = new Set(response.data.videos.map((v) => v.bvid));
			}
		} catch (error) {
			console.error('加载已下载视频失败:', error);
		} finally {
			loadingDownloaded = false;
		}
	}

	// 加载UP主投稿列表
	async function loadSubmissionVideos() {
		submissionLoading = true;
		submissionError = null;
		submissionVideos = [];
		currentLoadedPage = 0;
		hasMoreVideos = true;
		showLoadMoreButton = false;

		try {
			await loadVideosInBatch(INITIAL_LOAD_SIZE);
		} catch (err) {
			submissionError = err instanceof Error ? err.message : '网络请求失败';
		} finally {
			submissionLoading = false;
		}
	}

	// 批量加载视频
	async function loadVideosInBatch(loadCount: number) {
		const startPage = currentLoadedPage + 1;
		const targetVideos = Math.min(
			submissionVideos.length + loadCount,
			submissionTotalCount || Infinity
		);
		const neededPages = Math.ceil(targetVideos / SUBMISSION_PAGE_SIZE);

		for (let page = startPage; page <= neededPages; page++) {
			loadingProgress = `正在加载第 ${page} 页...`;

			if (page > startPage) {
				await new Promise((resolve) => setTimeout(resolve, PAGE_DELAY));
			}

			const response = await api.getSubmissionVideos({
				up_id: upperId.toString(),
				page: page,
				page_size: SUBMISSION_PAGE_SIZE
			});

			if (!response.data) {
				throw new Error('获取投稿列表失败');
			}

			if (page === 1 && submissionTotalCount === 0) {
				submissionTotalCount = response.data.total;
			}

			const newVideos = response.data.videos || [];
			const existingBvids = new Set(submissionVideos.map((v) => v.bvid));
			const uniqueNewVideos = newVideos.filter((video) => !existingBvids.has(video.bvid));

			submissionVideos = [...submissionVideos, ...uniqueNewVideos];
			currentLoadedPage = page;

			if (
				submissionVideos.length >= targetVideos ||
				submissionVideos.length >= submissionTotalCount
			) {
				break;
			}
		}

		hasMoreVideos = submissionVideos.length < submissionTotalCount;
		loadingProgress = '';

		// 更新过滤后的视频列表
		updateFilteredVideos();
	}

	// 更新过滤后的视频列表（排除已下载的）
	function updateFilteredVideos() {
		if (submissionSearchQuery.trim()) {
			return;
		}
		// 如果还在加载已下载视频列表，跳过过滤（等待resetAndLoad最后统一调用）
		if (loadingDownloaded) {
			return;
		}
		// 排除已下载的视频
		filteredSubmissionVideos = submissionVideos.filter((video) => !downloadedBvids.has(video.bvid));

		// 如果过滤后的视频很少（少于10个）但还有更多视频可加载，显示"加载更多"按钮
		if (filteredSubmissionVideos.length < 10 && hasMoreVideos && !isLoadingMore) {
			showLoadMoreButton = true;
		}
	}

	// 监听搜索查询变化
	let searchDebounceTimer: ReturnType<typeof setTimeout> | null = null;
	$: if (isOpen && submissionSearchQuery !== undefined) {
		if (searchDebounceTimer) {
			clearTimeout(searchDebounceTimer);
		}
		searchDebounceTimer = setTimeout(() => {
			handleSearchChange();
		}, 300);
	}

	async function handleSearchChange() {
		if (!submissionSearchQuery.trim()) {
			updateFilteredVideos();
			return;
		}

		isSearching = true;
		try {
			const response = await api.getSubmissionVideos({
				up_id: upperId.toString(),
				page: 1,
				page_size: 30,
				keyword: submissionSearchQuery.trim()
			});

			if (response.data && response.data.videos) {
				// 排除已下载的视频
				filteredSubmissionVideos = response.data.videos.filter(
					(video) => !downloadedBvids.has(video.bvid)
				);
			} else {
				filteredSubmissionVideos = [];
			}
		} catch (error) {
			console.error('搜索视频失败:', error);
			toast.error('搜索失败', { description: '请稍后重试' });
			filteredSubmissionVideos = submissionVideos
				.filter((video) => !downloadedBvids.has(video.bvid))
				.filter((video) =>
					video.title.toLowerCase().includes(submissionSearchQuery.toLowerCase().trim())
				);
		} finally {
			isSearching = false;
		}
	}

	// 加载更多投稿视频
	async function loadMoreSubmissionVideos() {
		if (!hasMoreVideos || isLoadingMore) return;

		isLoadingMore = true;
		showLoadMoreButton = false;
		try {
			await loadVideosInBatch(LOAD_MORE_SIZE);
		} catch (err) {
			console.error('加载更多视频失败:', err);
			toast.error('加载更多视频失败', {
				description: err instanceof Error ? err.message : '网络请求失败'
			});
		} finally {
			isLoadingMore = false;
		}
	}

	// 处理滚动事件
	function handleSubmissionScroll(event: Event) {
		const container = event.target as HTMLElement;
		if (!container || !hasMoreVideos) return;

		const { scrollTop, scrollHeight, clientHeight } = container;
		const threshold = 100;

		if (scrollHeight - scrollTop - clientHeight < threshold) {
			showLoadMoreButton = true;
		}
	}

	// 切换视频选择
	function toggleSubmissionVideo(bvid: string) {
		if (selectedSubmissionVideos.has(bvid)) {
			selectedSubmissionVideos.delete(bvid);
		} else {
			selectedSubmissionVideos.add(bvid);
		}
		selectedSubmissionVideos = selectedSubmissionVideos;
	}

	// 全选
	function selectAllSubmissions() {
		filteredSubmissionVideos.forEach((video) => selectedSubmissionVideos.add(video.bvid));
		selectedSubmissionVideos = selectedSubmissionVideos;
	}

	// 全不选
	function selectNoneSubmissions() {
		filteredSubmissionVideos.forEach((video) => selectedSubmissionVideos.delete(video.bvid));
		selectedSubmissionVideos = selectedSubmissionVideos;
	}

	// 反选
	function invertSubmissionSelection() {
		filteredSubmissionVideos.forEach((video) => {
			if (selectedSubmissionVideos.has(video.bvid)) {
				selectedSubmissionVideos.delete(video.bvid);
			} else {
				selectedSubmissionVideos.add(video.bvid);
			}
		});
		selectedSubmissionVideos = selectedSubmissionVideos;
	}

	// 确认选择
	function confirmSubmissionSelection() {
		const selectedVideos = Array.from(selectedSubmissionVideos);
		dispatch('confirm', selectedVideos);
		isOpen = false;
	}

	// 取消选择
	function cancelSubmissionSelection() {
		dispatch('cancel');
		isOpen = false;
	}

	// 计算未下载的视频数量
	$: notDownloadedCount = submissionVideos.filter((v) => !downloadedBvids.has(v.bvid)).length;
</script>

<AlertDialog.Root bind:open={isOpen}>
	<AlertDialog.Content
		class="flex max-h-[85vh] !w-[95vw] !max-w-none flex-col overflow-hidden sm:max-h-[90vh] sm:!w-[85vw]"
	>
		<AlertDialog.Header
			class="-m-6 mb-0 flex-shrink-0 border-b bg-blue-50 p-3 sm:p-4 dark:bg-blue-950"
		>
			<div class="flex items-center justify-between">
				<div>
					<AlertDialog.Title class="flex items-center gap-2 text-blue-800 dark:text-blue-200">
						<span class="text-base sm:text-lg">选择历史投稿</span>
					</AlertDialog.Title>
					<AlertDialog.Description class="mt-1 text-xs text-blue-600 sm:text-sm dark:text-blue-400">
						{#if submissionLoading && submissionVideos.length === 0}
							正在加载...
						{:else if submissionTotalCount > 0}
							UP主 "{upperName}" 共 {submissionTotalCount} 个投稿，
							{#if loadingDownloaded}
								正在检查已下载...
							{:else}
								已下载 {downloadedBvids.size} 个，
								<span class="font-medium text-purple-600 dark:text-purple-400">
									可选 {notDownloadedCount} 个
								</span>
							{/if}
						{:else}
							暂无投稿
						{/if}
					</AlertDialog.Description>
				</div>
				<button
					type="button"
					onclick={cancelSubmissionSelection}
					class="p-1 text-xl text-blue-500 hover:text-blue-700 dark:text-blue-300 dark:hover:text-blue-100"
				>
					<X class="h-5 w-5" />
				</button>
			</div>
		</AlertDialog.Header>

		<div class="mt-4 flex min-h-0 flex-1 flex-col overflow-hidden">
			{#if submissionError}
				<div class="m-3 rounded-lg border border-red-200 bg-red-50 p-4">
					<div class="flex items-center gap-2">
						<svg class="h-5 w-5 text-red-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								stroke-width="2"
								d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
							/>
						</svg>
						<span class="text-sm font-medium text-red-800 dark:text-red-200">加载失败</span>
					</div>
					<p class="mt-1 text-sm text-red-700 dark:text-red-300">{submissionError}</p>
					<button
						type="button"
						class="mt-2 text-sm text-red-600 underline hover:text-red-800 dark:text-red-400 dark:hover:text-red-200"
						onclick={loadSubmissionVideos}
					>
						重试
					</button>
				</div>
			{:else}
				<!-- 搜索和操作栏 -->
				<div class="flex-shrink-0 space-y-2 px-1 sm:space-y-3">
					<div class="flex gap-2">
						<div class="relative flex-1">
							<input
								type="text"
								bind:value={submissionSearchQuery}
								placeholder="搜索视频标题..."
								class="w-full rounded-md border border-gray-300 px-2 py-1.5 pr-8 text-xs focus:border-blue-500 focus:ring-2 focus:ring-blue-500 focus:outline-none sm:px-3 sm:py-2 sm:text-sm dark:border-gray-600 dark:bg-gray-700 dark:text-white"
								disabled={isSearching}
							/>
							{#if isSearching}
								<div class="absolute inset-y-0 right-0 flex items-center pr-3">
									<svg class="h-4 w-4 animate-spin text-blue-600" fill="none" viewBox="0 0 24 24">
										<circle
											class="opacity-25"
											cx="12"
											cy="12"
											r="10"
											stroke="currentColor"
											stroke-width="4"
										></circle>
										<path
											class="opacity-75"
											fill="currentColor"
											d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
										></path>
									</svg>
								</div>
							{/if}
						</div>
					</div>

					{#if submissionSearchQuery.trim()}
						<div class="px-1 text-xs text-blue-600">
							{isSearching
								? '搜索中...'
								: `搜索结果：在UP主所有视频中搜索 "${submissionSearchQuery}"`}
						</div>
					{/if}

					<div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
						<div class="flex gap-1 sm:gap-2">
							<button
								type="button"
								class="bg-card text-foreground hover:bg-muted rounded-md border border-gray-300 px-2 py-1 text-xs font-medium sm:px-3 sm:text-sm dark:border-gray-600"
								onclick={selectAllSubmissions}
								disabled={filteredSubmissionVideos.length === 0}
							>
								全选
							</button>
							<button
								type="button"
								class="bg-card text-foreground hover:bg-muted rounded-md border border-gray-300 px-2 py-1 text-xs font-medium sm:px-3 sm:text-sm dark:border-gray-600"
								onclick={selectNoneSubmissions}
								disabled={selectedSubmissionCount === 0}
							>
								全不选
							</button>
							<button
								type="button"
								class="bg-card text-foreground hover:bg-muted rounded-md border border-gray-300 px-2 py-1 text-xs font-medium sm:px-3 sm:text-sm dark:border-gray-600"
								onclick={invertSubmissionSelection}
								disabled={filteredSubmissionVideos.length === 0}
							>
								反选
							</button>
						</div>

						<div class="text-muted-foreground text-xs sm:text-sm">
							已选择 {selectedSubmissionCount} / {filteredSubmissionVideos.length} 个视频
						</div>
					</div>
				</div>

				<!-- 视频列表 -->
				<div
					class="mt-3 min-h-0 flex-1 overflow-y-auto px-1"
					bind:this={submissionScrollContainer}
					onscroll={handleSubmissionScroll}
				>
					{#if (submissionLoading || loadingDownloaded) && submissionVideos.length === 0}
						<div class="flex items-center justify-center py-8">
							<svg
								class="h-8 w-8 animate-spin text-blue-600 dark:text-blue-400"
								fill="none"
								viewBox="0 0 24 24"
							>
								<circle
									class="opacity-25"
									cx="12"
									cy="12"
									r="10"
									stroke="currentColor"
									stroke-width="4"
								></circle>
								<path
									class="opacity-75"
									fill="currentColor"
									d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
								></path>
							</svg>
							<span class="text-muted-foreground ml-2 text-sm">加载中...</span>
						</div>
					{:else if filteredSubmissionVideos.length === 0}
						<div class="text-muted-foreground flex flex-col items-center justify-center py-8">
							<svg class="mb-2 h-12 w-12" fill="none" stroke="currentColor" viewBox="0 0 24 24">
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									stroke-width="2"
									d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
								/>
							</svg>
							<p class="text-sm">
								{#if hasMoreVideos}
									当前加载的 {submissionVideos.length} 个视频都已下载
								{:else if downloadedBvids.size > 0 && notDownloadedCount === 0}
									所有历史投稿已下载完成
								{:else}
									没有找到未下载的视频
								{/if}
							</p>
							{#if hasMoreVideos}
								<button
									type="button"
									class="mt-4 rounded-md border border-transparent bg-blue-600 px-6 py-2 text-sm font-medium text-white hover:bg-blue-700 focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
									onclick={loadMoreSubmissionVideos}
									disabled={isLoadingMore}
								>
									{#if isLoadingMore}
										<div class="flex items-center gap-2">
											<div
												class="h-4 w-4 animate-spin rounded-full border-2 border-white border-t-transparent"
											></div>
											<span>加载中...</span>
										</div>
									{:else}
										加载更多历史投稿 ({submissionVideos.length}/{submissionTotalCount})
									{/if}
								</button>
							{/if}
						</div>
					{:else}
						<div
							class="grid gap-3"
							style="grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));"
						>
							{#each filteredSubmissionVideos as video (video.bvid)}
								<div
									class="hover:bg-muted relative cursor-pointer rounded-lg border p-2 transition-all duration-300 hover:shadow-md {selectedSubmissionVideos.has(
										video.bvid
									)
										? 'border-blue-300 bg-blue-50 dark:border-blue-600 dark:bg-blue-950'
										: 'border-gray-200 dark:border-gray-700'}"
									onclick={() => toggleSubmissionVideo(video.bvid)}
									role="button"
									tabindex="0"
									onkeydown={(e) => e.key === 'Enter' && toggleSubmissionVideo(video.bvid)}
								>
									<div class="relative">
										<BiliImage
											src={video.cover}
											alt={video.title}
											class="aspect-video w-full rounded object-cover"
											placeholder="无封面"
										/>
										<input
											type="checkbox"
											checked={selectedSubmissionVideos.has(video.bvid)}
											onchange={() => toggleSubmissionVideo(video.bvid)}
											onclick={(e) => e.stopPropagation()}
											class="absolute top-1 right-1 z-10 h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500 dark:text-blue-400"
										/>
									</div>
									<div class="mt-2">
										<h4
											class="text-foreground line-clamp-2 text-xs leading-tight font-medium"
											title={video.title}
										>
											{video.title}
										</h4>
										<div class="text-muted-foreground mt-1 flex items-center gap-1 text-[10px]">
											<span>{formatSubmissionPlayCount(video.view)}播放</span>
											<span>·</span>
											<span>{formatSubmissionDate(video.pubtime)}</span>
										</div>
									</div>
								</div>
							{/each}
						</div>

						{#if submissionVideos.length > 0}
							{#if showLoadMoreButton && hasMoreVideos}
								<div class="py-4 text-center">
									<button
										type="button"
										class="rounded-md border border-transparent bg-blue-600 px-6 py-2 text-sm font-medium text-white hover:bg-blue-700 focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
										onclick={loadMoreSubmissionVideos}
										disabled={isLoadingMore}
									>
										{#if isLoadingMore}
											<div class="flex items-center gap-2">
												<div
													class="h-4 w-4 animate-spin rounded-full border-2 border-white border-t-transparent"
												></div>
												<span>加载中...</span>
											</div>
										{:else}
											加载更多 ({submissionVideos.length}/{submissionTotalCount})
										{/if}
									</button>
								</div>
							{:else if submissionTotalCount > 0 && !hasMoreVideos}
								<div class="text-muted-foreground py-4 text-center text-sm">
									已加载全部 {submissionTotalCount} 个视频（排除已下载 {downloadedBvids.size} 个）
								</div>
							{/if}
						{/if}
					{/if}
				</div>
			{/if}
		</div>

		<!-- 底部操作栏 -->
		<AlertDialog.Footer
			class="-mx-6 mt-4 -mb-6 flex flex-shrink-0 flex-col justify-end gap-2 border-t px-4 pt-3 pb-4 sm:flex-row sm:gap-3 sm:px-6 sm:pt-4 sm:pb-6"
		>
			<Button variant="outline" onclick={cancelSubmissionSelection} class="w-full sm:w-auto"
				>取消</Button
			>
			<Button onclick={confirmSubmissionSelection} class="w-full sm:w-auto">
				确认选择 ({selectedSubmissionVideos.size} 个视频)
			</Button>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
