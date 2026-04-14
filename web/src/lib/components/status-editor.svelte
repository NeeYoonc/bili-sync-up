<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';
	import {
		Dialog,
		DialogContent,
		DialogDescription,
		DialogFooter,
		DialogHeader,
		DialogTitle
	} from '$lib/components/ui/dialog/index.js';
	import { Tabs, TabsContent, TabsList, TabsTrigger } from '$lib/components/ui/tabs/index.js';
	import StatusTaskCard from './status-task-card.svelte';
	import type { VideoInfo, PageInfo, StatusUpdate, UpdateVideoStatusRequest } from '$lib/types';
	import { toast } from 'svelte-sonner';

	export let open = false;
	export let video: VideoInfo;
	export let pages: PageInfo[] = [];
	export let loading = false;
	export let onsubmit: (request: UpdateVideoStatusRequest) => void;

	$: videoTaskNames = (() => {
		const isBangumi = video.bangumi_title !== undefined;
		if (isBangumi) {
			return ['视频封面', '视频信息', 'tvshow.nfo', 'UP主信息', '分P下载'];
		}
		return ['视频封面', '视频信息', 'UP主头像', 'UP主信息', '分P下载'];
	})();

	const pageTaskNames = ['视频封面', '视频内容', '视频信息', '视频弹幕', '视频字幕'];

	let videoStatuses: number[] = [];
	let pageStatuses: Record<number, number[]> = {};
	let originalVideoStatuses: number[] = [];
	let originalPageStatuses: Record<number, number[]> = {};
	let selectedPageId: number | null = null;
	let activeView = 'video';
	let updateTrigger = 0;

	$: {
		videoStatuses = [...video.download_status];
		originalVideoStatuses = [...video.download_status];

		if (pages.length > 0) {
			pageStatuses = pages.reduce(
				(acc, page) => {
					acc[page.id] = [...page.download_status];
					return acc;
				},
				{} as Record<number, number[]>
			);
			originalPageStatuses = pages.reduce(
				(acc, page) => {
					acc[page.id] = [...page.download_status];
					return acc;
				},
				{} as Record<number, number[]>
			);
		} else {
			pageStatuses = {};
			originalPageStatuses = {};
		}
	}

	$: {
		if (pages.length === 0) {
			selectedPageId = null;
			activeView = 'video';
		} else if (selectedPageId === null || !pages.some((page) => page.id === selectedPageId)) {
			selectedPageId = pages[0].id;
		}
	}

	$: selectedPage = selectedPageId === null ? null : pages.find((page) => page.id === selectedPageId) ?? null;
	$: if (activeView === 'page' && pages.length === 0) activeView = 'video';
	$: buttonEnabled = updateTrigger >= 0 && hasAnyChanges();

	function resetVideoTask(taskIndex: number) {
		const originalValue = originalVideoStatuses[taskIndex];
		videoStatuses[taskIndex] = originalValue;
		videoStatuses = [...videoStatuses];
		updateTrigger++;
	}

	function resetPageTask(pageId: number, taskIndex: number) {
		if (!pageStatuses[pageId]) {
			pageStatuses[pageId] = [];
		}
		const originalValue = originalPageStatuses[pageId]?.[taskIndex] ?? 0;
		pageStatuses[pageId][taskIndex] = originalValue;
		pageStatuses = { ...pageStatuses };

		if (originalValue === 0 && videoStatuses[4] !== 0) {
			videoStatuses[4] = 0;
			videoStatuses = [...videoStatuses];
		} else if (originalValue === 7) {
			const allPagesCompleted = pages.every((page) => {
				const currentStatuses = pageStatuses[page.id] || [];
				return currentStatuses.every((status) => status === 7);
			});

			if (allPagesCompleted && videoStatuses[4] !== 7) {
				videoStatuses[4] = 7;
				videoStatuses = [...videoStatuses];
			}
		}

		updateTrigger++;
	}

	function handleVideoStatusChange(taskIndex: number, newValue: number) {
		videoStatuses[taskIndex] = newValue;
		videoStatuses = [...videoStatuses];
		updateTrigger++;
	}

	function handlePageStatusChange(pageId: number, taskIndex: number, newValue: number) {
		if (!pageStatuses[pageId]) {
			pageStatuses[pageId] = [];
		}
		pageStatuses[pageId][taskIndex] = newValue;
		pageStatuses = { ...pageStatuses };

		if (newValue === 0 && videoStatuses[4] !== 0) {
			videoStatuses[4] = 0;
			videoStatuses = [...videoStatuses];
		} else if (newValue === 7) {
			const allPagesCompleted = pages.every((page) => {
				const currentStatuses = pageStatuses[page.id] || [];
				return currentStatuses.every((status) => status === 7);
			});

			if (allPagesCompleted && videoStatuses[4] !== 7) {
				videoStatuses[4] = 7;
				videoStatuses = [...videoStatuses];
			}
		}

		updateTrigger++;
	}

	function resetAllStatuses() {
		videoStatuses = [...originalVideoStatuses];
		pageStatuses = {};
		Object.keys(originalPageStatuses).forEach((pageId) => {
			pageStatuses[parseInt(pageId)] = [...originalPageStatuses[parseInt(pageId)]];
		});
		updateTrigger++;
	}

	function hasVideoChanges(): boolean {
		return !videoStatuses.every((status, index) => status === originalVideoStatuses[index]);
	}

	function hasPageChanges(): boolean {
		return pages.some((page) => {
			const currentStatuses = pageStatuses[page.id] || [];
			const originalStatuses = originalPageStatuses[page.id] || [];
			return !currentStatuses.every((status, index) => status === originalStatuses[index]);
		});
	}

	function hasAnyChanges(): boolean {
		return hasVideoChanges() || hasPageChanges();
	}

	function buildRequest(): UpdateVideoStatusRequest {
		const request: UpdateVideoStatusRequest = {};

		if (hasVideoChanges()) {
			request.video_updates = [];
			videoStatuses.forEach((status, index) => {
				if (status !== originalVideoStatuses[index]) {
					request.video_updates!.push({
						status_index: index,
						status_value: status
					});
				}
			});
		}

		if (hasPageChanges()) {
			request.page_updates = [];
			pages.forEach((page) => {
				const currentStatuses = pageStatuses[page.id] || [];
				const originalStatuses = originalPageStatuses[page.id] || [];
				const updates: StatusUpdate[] = [];

				currentStatuses.forEach((status, index) => {
					if (status !== originalStatuses[index]) {
						updates.push({
							status_index: index,
							status_value: status
						});
					}
				});

				if (updates.length > 0) {
					request.page_updates!.push({
						page_id: page.id,
						updates
					});
				}
			});
		}

		return request;
	}

	function handleSubmit() {
		if (!hasAnyChanges()) {
			toast.info('没有状态变更需要提交');
			return;
		}

		onsubmit(buildRequest());
	}
</script>

<Dialog bind:open>
	<DialogContent
		interactOutsideBehavior="ignore"
		onInteractOutside={(e) => e.preventDefault()}
		class="status-editor-dialog flex !h-[92svh] !w-[calc(100vw-1rem)] !max-w-[calc(100vw-1rem)] flex-col overflow-hidden p-0 sm:!h-[min(88vh,920px)] sm:!w-[min(92vw,980px)] sm:!max-w-[min(92vw,980px)]"
	>
		<DialogHeader class="status-editor-header px-3 pt-2 pb-1 sm:px-6 sm:pt-5 sm:pb-2">
			<DialogTitle class="text-base sm:text-lg" title="编辑视频和分页的下载状态">
				编辑状态
			</DialogTitle>
			<DialogDescription class="status-editor-description text-muted-foreground space-y-2 text-sm">
				<div class="status-editor-copy text-[11px] leading-4 sm:text-sm sm:leading-6">
					修改视频和分页的下载状态。可以将任务重置为未开始状态，或者标记为已完成。
				</div>
				<div class="status-editor-desktop-warning hidden font-medium text-red-600 sm:block">
					⚠️ 已完成任务被重置为未开始，任务重新执行时会覆盖现存文件。
				</div>
				<div class="rounded-md border border-orange-200 bg-orange-50 px-3 py-2 text-orange-800 sm:hidden">
					<div class="space-y-1 text-[11px] leading-4">
						<div class="font-medium text-red-600">
							⚠️ 已完成任务被重置为未开始，任务重新执行时会覆盖现存文件。
						</div>
						<div>
							只有重置<strong>"分P下载"</strong
							>状态才会触发分页状态的重置，触发分页状态开始重新下载。
						</div>
					</div>
				</div>
				<div class="status-editor-desktop-reminder hidden rounded-md border border-orange-200 bg-orange-50 p-3 text-orange-800 sm:block">
					<div class="flex items-start gap-2">
						<span class="font-bold text-orange-600">💡</span>
						<div class="space-y-1">
							<div class="font-medium">重要提醒：</div>
							<div class="text-xs">
								只有重置<strong>"分P下载"</strong
								>状态才会触发分页状态的重置，触发分页状态开始重新下载！其他状态重置主要用于修复任务流程。
							</div>
						</div>
					</div>
				</div>
				<div class="status-editor-short-warning rounded-md border border-orange-200 bg-orange-50 px-3 py-2 text-xs leading-4 text-orange-800">
					<div class="font-medium text-red-600">
						⚠️ 重置已完成任务会覆盖现存文件；只有重置“分P下载”才会重新触发分页下载。
					</div>
				</div>
			</DialogDescription>
		</DialogHeader>

		<div class="status-editor-body flex-1 overflow-hidden px-2 pb-2 sm:px-6 sm:pb-5">
			<Tabs bind:value={activeView} class="status-editor-tabs flex h-full min-h-0 flex-col gap-2 sm:gap-4">
				<TabsList class="status-editor-tabs-list grid h-9 w-full grid-cols-[minmax(0,1fr)_minmax(0,1fr)] sm:h-10">
					<TabsTrigger
						value="video"
						class="min-w-0"
						title="查看和修改视频级任务状态"
					>
						视频状态
					</TabsTrigger>
					<TabsTrigger
						value="page"
						class="min-w-0"
						disabled={pages.length === 0}
						title="查看和修改分页级任务状态"
					>
						分页状态
					</TabsTrigger>
				</TabsList>

				<TabsContent value="video" class="min-h-0 flex-1 overflow-hidden">
					<div class="status-editor-panel bg-card h-full min-h-0 overflow-y-auto overscroll-contain rounded-lg border p-2.5 sm:p-4">
						<div class="status-editor-panel-list space-y-2.5 sm:space-y-3">
							{#each videoTaskNames as taskName, index (index)}
								<StatusTaskCard
									{taskName}
									currentStatus={videoStatuses[index] ?? 0}
									originalStatus={originalVideoStatuses[index] ?? 0}
									onStatusChange={(newStatus) => handleVideoStatusChange(index, newStatus)}
									onReset={() => resetVideoTask(index)}
									disabled={loading}
								/>
							{/each}
						</div>
					</div>
				</TabsContent>

				<TabsContent value="page" class="min-h-0 flex-1 overflow-hidden">
					{#if pages.length > 0 && selectedPage}
						<div class="flex h-full min-h-0 flex-col gap-2 sm:gap-3">
							{#if pages.length > 1}
								<div class="overflow-x-auto pb-1">
									<div class="flex min-w-max gap-2">
										{#each pages as page (page.id)}
											<Button
												variant={selectedPageId === page.id ? 'default' : 'outline'}
												size="sm"
												onclick={() => (selectedPageId = page.id)}
												class="max-w-[220px] cursor-pointer justify-start truncate"
												title={`P${page.pid}: ${page.name}`}
											>
												P{page.pid}
											</Button>
										{/each}
									</div>
								</div>
							{/if}

							<div class="status-editor-page-summary rounded-lg border bg-muted/30 px-2.5 py-2 sm:px-3">
								<div class="text-sm font-medium">P{selectedPage.pid}</div>
								<div
									class="text-muted-foreground truncate text-xs sm:text-sm"
									title={selectedPage.name}
								>
									{selectedPage.name}
								</div>
							</div>

							<div class="status-editor-panel bg-card min-h-0 flex-1 overflow-y-auto overscroll-contain rounded-lg border p-2.5 sm:p-4">
								<div class="status-editor-panel-list space-y-2.5 sm:space-y-3">
									{#each pageTaskNames as taskName, index (index)}
										<StatusTaskCard
											{taskName}
											currentStatus={(pageStatuses[selectedPage.id] || selectedPage.download_status)[index] ?? 0}
											originalStatus={originalPageStatuses[selectedPage.id]?.[index] ?? 0}
											onStatusChange={(newStatus) =>
												handlePageStatusChange(selectedPage.id, index, newStatus)}
											onReset={() => resetPageTask(selectedPage.id, index)}
											disabled={loading}
										/>
									{/each}
								</div>
							</div>
						</div>
					{/if}
				</TabsContent>
			</Tabs>
		</div>

		<DialogFooter class="status-editor-footer bg-background !grid !grid-cols-2 gap-2 border-t px-2 py-2 sm:!flex sm:px-6 sm:pt-4 sm:pb-5">
			<Button
				variant="outline"
				onclick={resetAllStatuses}
				disabled={!buttonEnabled}
				class="status-editor-footer-button h-10 w-full cursor-pointer text-sm sm:h-10 sm:flex-1"
				title="将当前修改恢复为原始状态"
			>
				重置所有状态
			</Button>
			<Button
				onclick={handleSubmit}
				disabled={loading || !buttonEnabled}
				class="status-editor-footer-button h-10 w-full cursor-pointer text-sm sm:h-10 sm:flex-1"
				title="提交当前状态修改"
			>
				{loading ? '提交中...' : '提交更改'}
			</Button>
		</DialogFooter>
	</DialogContent>
</Dialog>

<style>
	.status-editor-short-warning {
		display: none;
	}

	@media (max-height: 700px) {
		:global(.status-editor-dialog) {
			height: min(94vh, 720px) !important;
		}

		:global(.status-editor-header) {
			padding: 0.75rem 1rem 0.375rem;
		}

		:global(.status-editor-description) {
			gap: 0.375rem;
		}

		.status-editor-copy {
			font-size: 0.75rem;
			line-height: 1.125rem;
		}

		.status-editor-desktop-warning,
		.status-editor-desktop-reminder {
			display: none !important;
		}

		.status-editor-short-warning {
			display: block;
		}

		.status-editor-body {
			padding: 0 0.75rem 0.5rem;
		}

		:global(.status-editor-tabs) {
			gap: 0.5rem;
		}

		:global(.status-editor-tabs-list) {
			height: 2.25rem;
		}

		.status-editor-panel,
		.status-editor-page-summary {
			padding: 0.625rem;
		}

		.status-editor-panel-list {
			gap: 0.5rem;
		}

		:global(.status-editor-footer) {
			padding: 0.5rem 0.75rem 0.75rem;
		}

		:global(.status-editor-footer-button) {
			height: 2.25rem;
		}
	}
</style>
