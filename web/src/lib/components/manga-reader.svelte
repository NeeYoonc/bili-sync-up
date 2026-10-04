<script lang="ts">
	import api from '$lib/api';
	import { Button } from '$lib/components/ui/button/index.js';
	import Loading from '$lib/components/ui/Loading.svelte';
	import type { ApiError, MangaChapterResponse } from '$lib/types';
	import ChevronLeftIcon from '@lucide/svelte/icons/chevron-left';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import ChevronsLeftIcon from '@lucide/svelte/icons/chevrons-left';
	import ChevronsRightIcon from '@lucide/svelte/icons/chevrons-right';
	import RowsIcon from '@lucide/svelte/icons/rows-3';
	import SquareIcon from '@lucide/svelte/icons/square';
	import StretchHorizontalIcon from '@lucide/svelte/icons/stretch-horizontal';
	import MoveHorizontalIcon from '@lucide/svelte/icons/move-horizontal';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import AlertCircleIcon from '@lucide/svelte/icons/alert-circle';
	import BookOpenIcon from '@lucide/svelte/icons/book-open';
	import MaximizeIcon from '@lucide/svelte/icons/maximize';
	import MinimizeIcon from '@lucide/svelte/icons/minimize';
	import { onDestroy } from 'svelte';

	// 当前阅读的漫画话：一个 video 对应一个 CBZ 压缩包，CBZ 内的图片才是一页
	export let videoId: number;
	export let chapterTitle = '';

	const PROGRESS_KEY_PREFIX = 'manga:progress:';

	let manifest: MangaChapterResponse | null = null;
	let loading = false;
	let loadError: string | null = null;
	let pageIndex = 0;
	// 单页模式下是否按宽度铺满（默认按高度完整显示整页）
	let fitWidth = false;
	// 连续滚动（长图）模式
	let scrollMode = false;
	let imageLoading = false;
	let imageFailed = false;
	// 手动重试时用它绕开浏览器缓存
	let retryToken = 0;

	let scrollContainer: HTMLDivElement | null = null;
	// 全屏阅读：优先用 Fullscreen API，浏览器不支持时退回「伪全屏」（固定在视口上）
	let readerRoot: HTMLDivElement | null = null;
	let isFullscreen = false;
	let pseudoFullscreen = false;
	$: fullscreenActive = isFullscreen || pseudoFullscreen;
	let pageElements: (HTMLDivElement | null)[] = [];
	let scrollQueued = false;
	let touchStartX = 0;
	let touchStartY = 0;

	$: pageCount = manifest?.pages.length ?? 0;
	$: currentPage = manifest?.pages[pageIndex] ?? null;
	$: canPrev = pageIndex > 0;
	$: canNext = pageCount > 0 && pageIndex < pageCount - 1;

	// videoId 变化时重新加载清单并回到该话上次的阅读位置
	let loadedVideoId = -1;
	$: if (videoId && videoId !== loadedVideoId) {
		loadedVideoId = videoId;
		pageIndex = 0;
		scrollMode = false;
		fitWidth = false;
		retryToken = 0;
		resetImageState();
		loadChapter();
	}

	function pageUrl(index: number) {
		const suffix = retryToken > 0 ? `?retry=${retryToken}` : '';
		return `/api/manga/page/${videoId}/${index}${suffix}`;
	}

	function resetImageState() {
		imageLoading = true;
		imageFailed = false;
	}

	function retryCurrentPage() {
		retryToken += 1;
		resetImageState();
	}

	async function loadChapter() {
		if (!videoId) return;
		const requestedVideoId = videoId;
		loading = true;
		loadError = null;
		manifest = null;
		try {
			const result = await api.getMangaChapter(requestedVideoId);
			if (requestedVideoId !== videoId) return;
			manifest = result.data;
			pageElements = new Array(result.data.pages.length).fill(null);
			const saved = readProgress(result.data.pages.length);
			pageIndex = saved ?? 0;
			resetImageState();
			saveProgress();
		} catch (error) {
			if (requestedVideoId !== videoId) return;
			console.error('加载漫画话失败:', error);
			loadError = (error as ApiError)?.message || '读取漫画话失败';
		} finally {
			if (requestedVideoId === videoId) {
				loading = false;
			}
		}
	}

	function progressKey() {
		return `${PROGRESS_KEY_PREFIX}${videoId}`;
	}

	function readProgress(total: number): number | null {
		try {
			const raw = localStorage.getItem(progressKey());
			if (!raw) return null;
			const value = Number.parseInt(raw, 10);
			if (!Number.isFinite(value) || value <= 0 || value >= total) return null;
			return value;
		} catch {
			return null;
		}
	}

	function saveProgress() {
		if (typeof localStorage === 'undefined') return;
		try {
			localStorage.setItem(progressKey(), String(pageIndex));
		} catch {
			// 隐私模式等场景下写入失败可以忽略
		}
	}

	function goToPage(target: number, options?: { silent?: boolean }) {
		if (pageCount <= 0) return;
		const next = Math.min(Math.max(target, 0), pageCount - 1);
		if (next === pageIndex) return;
		pageIndex = next;
		retryToken = 0;
		resetImageState();
		saveProgress();
		if (scrollMode) scrollToPage(next, options?.silent ? 'auto' : 'smooth');
	}

	function nextPage() {
		goToPage(pageIndex + 1);
	}

	function prevPage() {
		goToPage(pageIndex - 1);
	}

	function jumpToPage(value: string) {
		const parsed = Number.parseInt(value, 10);
		if (!Number.isFinite(parsed)) return;
		goToPage(parsed - 1);
	}

	function toggleScrollMode() {
		scrollMode = !scrollMode;
		if (scrollMode) {
			// 等切换后的图片挂载完成再对齐到当前页
			setTimeout(() => scrollToPage(pageIndex), 0);
		}
	}

	function scrollToPage(index: number, behavior: ScrollBehavior = 'auto') {
		const container = scrollContainer;
		const element = pageElements[index];
		if (!container || !element) return;
		container.scrollTo({ top: element.offsetTop, behavior });
	}

	function handleScroll() {
		if (!scrollMode || scrollQueued) return;
		scrollQueued = true;
		requestAnimationFrame(() => {
			scrollQueued = false;
			syncPageFromScroll();
		});
	}

	function syncPageFromScroll() {
		const container = scrollContainer;
		if (!container || pageCount <= 0) return;
		const middle = container.scrollTop + container.clientHeight / 2;
		let candidate = 0;
		for (let i = 0; i < pageElements.length; i += 1) {
			const element = pageElements[i];
			if (!element) continue;
			if (element.offsetTop <= middle) {
				candidate = i;
			} else {
				break;
			}
		}
		if (candidate !== pageIndex) {
			pageIndex = candidate;
			saveProgress();
		}
	}

	type FullscreenDocument = Document & {
		webkitFullscreenElement?: Element | null;
		webkitExitFullscreen?: () => Promise<void> | void;
	};

	type FullscreenElement = HTMLElement & {
		webkitRequestFullscreen?: () => Promise<void> | void;
		webkitRequestFullScreen?: () => Promise<void> | void;
	};

	function syncFullscreenState() {
		if (typeof document === 'undefined') return;
		const doc = document as FullscreenDocument;
		const element = doc.fullscreenElement ?? doc.webkitFullscreenElement ?? null;
		isFullscreen = !!element && element === readerRoot;
	}

	function lockBodyScroll(locked: boolean) {
		if (typeof document === 'undefined') return;
		if (locked) {
			document.body.style.setProperty('overflow', 'hidden');
		} else {
			document.body.style.removeProperty('overflow');
		}
	}

	async function exitFullscreen() {
		if (pseudoFullscreen) {
			pseudoFullscreen = false;
			lockBodyScroll(false);
		}
		if (typeof document !== 'undefined' && isFullscreen) {
			const doc = document as FullscreenDocument;
			try {
				if (typeof doc.exitFullscreen === 'function') {
					await doc.exitFullscreen();
				} else if (typeof doc.webkitExitFullscreen === 'function') {
					await doc.webkitExitFullscreen();
				}
			} catch {
				// 退出失败时按状态收尾即可，不用打断阅读
			}
		}
		isFullscreen = false;
	}

	async function toggleFullscreen() {
		if (isFullscreen || pseudoFullscreen) {
			await exitFullscreen();
			return;
		}
		const element = readerRoot as FullscreenElement | null;
		const request = element?.requestFullscreen ?? element?.webkitRequestFullscreen ?? element?.webkitRequestFullScreen;
		if (element && request) {
			try {
				await request.call(element);
				// requestFullscreen 成功后 fullscreenchange 是异步来的，这里先按成功记状态
				isFullscreen = true;
				return;
			} catch {
				// 某些浏览器（iOS Safari）对任意元素全屏直接拒绝，下面走伪全屏
			}
		}
		pseudoFullscreen = true;
		lockBodyScroll(true);
	}

	function handleKeydown(event: KeyboardEvent) {
		const target = event.target as HTMLElement | null;
		const tag = target?.tagName?.toLowerCase();
		if (tag === 'input' || tag === 'textarea' || tag === 'select' || target?.isContentEditable) return;
		if (event.ctrlKey || event.metaKey || event.altKey) return;
		if (event.key === 'Escape' && pseudoFullscreen) {
			event.preventDefault();
			void exitFullscreen();
			return;
		}
		switch (event.key) {
			case 'ArrowLeft':
			case 'PageUp':
				event.preventDefault();
				prevPage();
				break;
			case 'ArrowRight':
			case 'PageDown':
			case ' ':
				event.preventDefault();
				nextPage();
				break;
			case 'Home':
				event.preventDefault();
				goToPage(0);
				break;
			case 'End':
				event.preventDefault();
				goToPage(pageCount - 1);
				break;
			case 'f':
			case 'F':
				event.preventDefault();
				void toggleFullscreen();
				break;
			default:
				break;
		}
	}

	function handleTouchStart(event: TouchEvent) {
		const touch = event.changedTouches[0];
		if (!touch) return;
		touchStartX = touch.clientX;
		touchStartY = touch.clientY;
	}

	function handleTouchEnd(event: TouchEvent) {
		const touch = event.changedTouches[0];
		if (!touch) return;
		const deltaX = touch.clientX - touchStartX;
		const deltaY = touch.clientY - touchStartY;
		if (Math.abs(deltaX) < 60 || Math.abs(deltaX) < Math.abs(deltaY)) return;
		if (deltaX < 0) {
			nextPage();
		} else {
			prevPage();
		}
	}

	function getFileName(path: string) {
		if (!path) return '';
		return path.replace(/\\/g, '/').split('/').pop() ?? path;
	}

	function formatSize(bytes: number) {
		if (!Number.isFinite(bytes) || bytes <= 0) return '未知大小';
		const units = ['B', 'KB', 'MB', 'GB'];
		let value = bytes;
		let unitIndex = 0;
		while (value >= 1024 && unitIndex < units.length - 1) {
			value /= 1024;
			unitIndex += 1;
		}
		const digits = unitIndex === 0 ? 0 : 1;
		return `${value.toFixed(digits)} ${units[unitIndex]}`;
	}

	if (typeof window !== 'undefined') {
		window.addEventListener('keydown', handleKeydown);
	}
	if (typeof document !== 'undefined') {
		// 用户按 F11 / Esc 或浏览器自己退出全屏时同步状态
		document.addEventListener('fullscreenchange', syncFullscreenState);
		document.addEventListener('webkitfullscreenchange', syncFullscreenState);
	}
	onDestroy(() => {
		if (typeof window !== 'undefined') {
			window.removeEventListener('keydown', handleKeydown);
		}
		if (typeof document !== 'undefined') {
			document.removeEventListener('fullscreenchange', syncFullscreenState);
			document.removeEventListener('webkitfullscreenchange', syncFullscreenState);
			lockBodyScroll(false);
		}
	});
</script>

{#if loading && !manifest}
	<div class="flex justify-center rounded-lg bg-black py-16">
		<Loading text="正在读取漫画压缩包..." showSpinner />
	</div>
{:else if loadError}
	<div
		class="flex flex-col items-center gap-3 rounded-lg border border-destructive/40 bg-destructive/5 px-4 py-10 text-center"
	>
		<AlertCircleIcon class="text-destructive h-8 w-8" />
		<div class="text-sm font-medium">漫画读取失败</div>
		<div class="text-muted-foreground max-w-md text-xs break-all">{loadError}</div>
		<Button size="sm" variant="outline" onclick={loadChapter}>
			<RefreshCwIcon class="mr-2 h-4 w-4" />重试
		</Button>
	</div>
{:else if manifest}
	<div
		bind:this={readerRoot}
		class="space-y-3"
		class:manga-reader-root={fullscreenActive}
		class:manga-reader-pseudo={pseudoFullscreen}
	>
		<div class="bg-muted/50 flex flex-wrap items-center justify-between gap-2 rounded-lg px-3 py-2">
			<div class="min-w-0">
				<div class="flex items-center gap-2 text-sm font-medium">
					<BookOpenIcon class="h-4 w-4 shrink-0" />
					<span class="truncate" title={chapterTitle || manifest.title}>
						{chapterTitle || manifest.title}
					</span>
				</div>
				<div
					class="text-muted-foreground truncate text-xs"
					title={`${getFileName(manifest.path)} · ${formatSize(manifest.size_bytes)}`}
				>
					{getFileName(manifest.path)} · {formatSize(manifest.size_bytes)} · 共 {pageCount} 页
				</div>
			</div>
			<div class="flex items-center gap-1">
				<Button
					size="sm"
					variant={scrollMode ? 'outline' : 'default'}
					title="单页翻页模式"
					onclick={() => {
						if (scrollMode) toggleScrollMode();
					}}
				>
					<SquareIcon class="mr-1.5 h-4 w-4" />单页
				</Button>
				<Button
					size="sm"
					variant={scrollMode ? 'default' : 'outline'}
					title="连续滚动（整话长图）模式"
					onclick={() => {
						if (!scrollMode) toggleScrollMode();
					}}
				>
					<RowsIcon class="mr-1.5 h-4 w-4" />连续滚动
				</Button>
				<Button
					size="sm"
					variant="outline"
					title={fullscreenActive ? '退出全屏阅读（Esc）' : '全屏阅读，整页铺满屏幕（F）'}
					onclick={toggleFullscreen}
				>
					{#if fullscreenActive}
						<MinimizeIcon class="mr-1.5 h-4 w-4" />退出全屏
					{:else}
						<MaximizeIcon class="mr-1.5 h-4 w-4" />全屏
					{/if}
				</Button>
			</div>
		</div>

		{#if scrollMode}
			<div
				bind:this={scrollContainer}
				onscroll={handleScroll}
				ontouchstart={handleTouchStart}
				ontouchend={handleTouchEnd}
				class="manga-reader-area relative overflow-y-auto rounded-lg bg-black"
				role="group"
				aria-label="漫画连续滚动区域"
			>
				{#each manifest.pages as page (page.index)}
					<div bind:this={pageElements[page.index]} class="w-full">
						<img
							src={pageUrl(page.index)}
							alt={`${chapterTitle || manifest.title} 第 ${page.index + 1} 页`}
							class="mx-auto block h-auto w-full max-w-4xl"
							loading="lazy"
							draggable="false"
						/>
					</div>
				{/each}
			</div>
		{:else}
			<div
				class="manga-reader-area relative flex overflow-auto rounded-lg bg-black"
				role="group"
				aria-label="漫画单页阅读区域"
				ontouchstart={handleTouchStart}
				ontouchend={handleTouchEnd}
			>
				{#if currentPage}
					{#if imageLoading}
						<div class="absolute inset-0 flex items-center justify-center">
							<Loading text={`正在加载第 ${pageIndex + 1} 页...`} showSpinner size="sm" />
						</div>
					{/if}
					{#if imageFailed}
						<div
							class="absolute inset-0 flex flex-col items-center justify-center gap-3 bg-black/80 px-4 text-center text-white"
						>
							<AlertCircleIcon class="h-8 w-8" />
							<div class="text-sm">第 {pageIndex + 1} 页图片加载失败</div>
							<Button size="sm" variant="secondary" onclick={retryCurrentPage}>
								<RefreshCwIcon class="mr-2 h-4 w-4" />重新加载
							</Button>
						</div>
					{/if}
					{#key `${videoId}-${pageIndex}-${retryToken}`}
						<img
							src={pageUrl(pageIndex)}
							alt={`${chapterTitle || manifest.title} 第 ${pageIndex + 1} 页`}
							class={fitWidth
								? 'm-auto block h-auto w-full object-contain'
								: 'm-auto block h-auto max-h-full w-auto max-w-full object-contain'}
							class:opacity-0={imageLoading && !imageFailed}
							draggable="false"
							onload={() => {
								imageLoading = false;
							}}
							onerror={(event) => {
								console.warn('漫画分页加载失败:', event);
								imageLoading = false;
								imageFailed = true;
							}}
						/>
					{/key}
				{/if}
			</div>
		{/if}

		<div class="flex flex-wrap items-center justify-between gap-2">
			<div class="flex items-center gap-1">
				<Button
					size="sm"
					variant="outline"
					class="h-8 w-8 p-0"
					disabled={!canPrev}
					title="第一页"
					onclick={() => goToPage(0)}
				>
					<ChevronsLeftIcon class="h-4 w-4" />
				</Button>
				<Button
					size="sm"
					variant="outline"
					class="h-8 w-8 p-0"
					disabled={!canPrev}
					title="上一页（←）"
					onclick={prevPage}
				>
					<ChevronLeftIcon class="h-4 w-4" />
				</Button>
			</div>

			<div class="flex items-center gap-2">
				<input
					type="number"
					min="1"
					max={pageCount}
					value={pageIndex + 1}
					onchange={(event) => jumpToPage((event.currentTarget as HTMLInputElement).value)}
					class="border-input bg-background focus:ring-ring h-8 w-16 rounded-md border px-2 text-center text-sm focus:ring-2 focus:outline-none"
					title="输入页码后回车跳转"
				/>
				<span class="text-muted-foreground text-sm">/ {pageCount} 页</span>
			</div>

			<div class="flex items-center gap-1">
				<Button
					size="sm"
					variant="outline"
					class="h-8 w-8 p-0"
					disabled={!canNext}
					title="下一页（→）"
					onclick={nextPage}
				>
					<ChevronRightIcon class="h-4 w-4" />
				</Button>
				<Button
					size="sm"
					variant="outline"
					class="h-8 w-8 p-0"
					disabled={!canNext}
					title="最后一页"
					onclick={() => goToPage(pageCount - 1)}
				>
					<ChevronsRightIcon class="h-4 w-4" />
				</Button>
			</div>
		</div>

		{#if pageCount > 1}
			<input
				type="range"
				min="0"
				max={pageCount - 1}
				value={pageIndex}
				oninput={(event) => goToPage(Number((event.currentTarget as HTMLInputElement).value))}
				class="w-full accent-black dark:accent-white"
				title="拖动快速翻页"
			/>
		{/if}

		<div class="flex flex-wrap items-center justify-between gap-2 text-xs">
			<div class="text-muted-foreground flex items-center gap-3">
				<button
					type="button"
					class="hover:text-foreground flex items-center gap-1 transition-colors"
					title="单页模式下切换为按宽度铺满"
					onclick={() => (fitWidth = !fitWidth)}
					disabled={scrollMode}
				>
					{#if fitWidth}
						<MoveHorizontalIcon class="h-3.5 w-3.5" />适应宽度
					{:else}
						<StretchHorizontalIcon class="h-3.5 w-3.5" />适应高度
					{/if}
				</button>
				<span>← → 翻页 · 空格下一页 · F 全屏 · 触屏左右滑动</span>
			</div>
			{#if pageIndex > 0}
				<button
					type="button"
					class="text-muted-foreground hover:text-foreground transition-colors"
					title="从第一页重新开始阅读"
					onclick={() => goToPage(0)}
				>
					回到第 1 页
				</button>
			{/if}
		</div>
	</div>
{:else}
	<div class="rounded-lg bg-black px-4 py-10 text-center text-sm text-white">没有可阅读的页面</div>
{/if}

<style>
	/* 阅读区域高度跟着窗口走：减掉标题栏 / 翻页控件占的地方，保证整页图都能看见，
	 * 而不是只按 75vh 算导致图片和控件被挤到可视区域外面去。 */
	.manga-reader-area {
		height: clamp(240px, calc(100vh - 300px), 80vh);
		height: clamp(240px, calc(100dvh - 300px), 80vh);
	}

	/* 全屏阅读：整块铺满屏幕，图片区域吃掉标题栏 / 翻页控件之外的剩余高度 */
	.manga-reader-root {
		display: flex;
		flex-direction: column;
		height: 100%;
		padding: 0.75rem;
		overflow: hidden;
		background: var(--background);
	}

	.manga-reader-pseudo {
		position: fixed;
		inset: 0;
		z-index: 1000;
	}

	.manga-reader-root .manga-reader-area {
		flex: 1 1 auto;
		height: auto;
		min-height: 160px;
	}
</style>
