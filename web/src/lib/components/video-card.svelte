<script lang="ts">
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as AlertDialog from '$lib/components/ui/alert-dialog/index.js';
	import type { ApiError, VideoInfo } from '$lib/types';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import InfoIcon from '@lucide/svelte/icons/info';
	import UserIcon from '@lucide/svelte/icons/user';
	import { goto } from '$app/navigation';
	import api from '$lib/api';
	import * as Tooltip from '$lib/components/ui/tooltip/index.js';
	import { toast } from 'svelte-sonner';

	export let video: VideoInfo;
	export let showActions: boolean = true; // 控制是否显示操作按钮
	export let mode: 'default' | 'detail' | 'page' = 'default'; // 卡片模式
	export let customTitle: string = ''; // 自定义标题
	export let customSubtitle: string = ''; // 自定义副标题
	export let taskNames: string[] = []; // 自定义任务名称
	export let showProgress: boolean = true; // 是否显示进度信息
	export let progressHeight: string = 'h-2'; // 进度条高度
	export let gap: string = 'gap-1'; // 进度条间距
	export let onReset: ((force: boolean) => Promise<void>) | null = null; // 自定义重置函数
	export let resetDialogOpen = false; // 导出对话框状态，让父组件可以控制
	export let resetting = false;
	export let selectionMode: boolean = false; // 是否为选择模式
	export let selected: boolean = false; // 是否被选中
	export let onSelectionChange: ((videoId: number, selected: boolean) => void) | null = null; // 选择状态变化回调

	function shouldIgnoreSelectionToggle(target: EventTarget | null): boolean {
		if (!target || !(target instanceof HTMLElement)) return false;
		return Boolean(target.closest('button, a, input, textarea, select, label'));
	}

	function handleCardClick(event: MouseEvent) {
		if (!selectionMode || !onSelectionChange) return;
		if (shouldIgnoreSelectionToggle(event.target)) return;
		onSelectionChange(video.id, !selected);
	}

	function handleCardKeydown(event: KeyboardEvent) {
		if (!selectionMode || !onSelectionChange) return;
		if (shouldIgnoreSelectionToggle(event.target)) return;
		if (event.key !== 'Enter' && event.key !== ' ' && event.key !== 'Spacebar') return;
		event.preventDefault();
		onSelectionChange(video.id, !selected);
	}

	function getStatusText(status: number): string {
		if (status === 7) {
			return '已完成';
		} else if (status === 0) {
			return '未开始';
		} else {
			return `失败${status}次`;
		}
	}

	function getSegmentColor(status: number): string {
		if (status === 7) {
			return 'bg-green-500'; // 绿色 - 成功
		} else if (status === 0) {
			return 'bg-yellow-500'; // 黄色 - 未开始
		} else {
			return 'bg-red-500'; // 红色 - 失败
		}
	}

	function getOverallStatus(downloadStatus: number[]): {
		text: string;
		color: 'default' | 'secondary' | 'destructive' | 'outline';
	} {
		if (video.valid === false) {
			return { text: '无效', color: 'outline' };
		}

		const completed = downloadStatus.filter((status) => status === 7).length;
		const total = downloadStatus.length;
		const failed = downloadStatus.filter((status) => status !== 7 && status !== 0).length;

		if (completed === total) {
			return { text: '全部完成', color: 'default' };
		} else if (failed > 0) {
			return { text: '部分失败', color: 'destructive' };
		} else {
			return { text: '进行中', color: 'secondary' };
		}
	}

	function getTaskName(index: number): string {
		if (taskNames.length > 0) {
			return taskNames[index] || `任务${index + 1}`;
		}

		// 根据视频类型返回不同的任务名称
		const isBangumi = video.bangumi_title !== undefined;

		if (isBangumi) {
			// 番剧任务名称：VideoStatus[2] 对应 tvshow.nfo 生成
			const bangumiTaskNames = ['视频封面', '视频信息', 'tvshow.nfo', 'UP主信息', '分P下载'];
			return bangumiTaskNames[index] || `任务${index + 1}`;
		} else {
			// 普通视频任务名称：VideoStatus[2] 对应 UP主头像下载
			const defaultTaskNames = ['视频封面', '视频信息', 'UP主头像', 'UP主信息', '分P下载'];
			return defaultTaskNames[index] || `任务${index + 1}`;
		}
	}

	$: overallStatus = getOverallStatus(video.download_status);
	$: completed = video.download_status.filter((status) => status === 7).length;
	$: total = video.download_status.length;

	async function handleReset(force: boolean = false) {
		resetting = true;
		try {
			if (onReset) {
				await onReset(force);
			} else {
				const response = await api.resetVideo(video.id, force);
				// 根据返回结果显示不同的提示
				if (response.data.resetted) {
					toast.success('重置成功', {
						description: `已重置 ${response.data.pages.length} 个分P${force ? ' (强制重置)' : ''}`
					});
				} else {
					if (force) {
						toast.info('无任务可重置', {
							description: '该视频暂无任何任务'
						});
					} else {
						toast.info('重置无效', {
							description: '所有任务均成功，无需重置。如需重新下载，请使用强制重置。'
						});
					}
				}
				// 稍后刷新页面
				setTimeout(() => {
					window.location.reload();
				}, 1000);
			}
		} catch (error) {
			console.error('重置失败:', error);
			toast.error('重置失败', {
				description: (error as ApiError).message
			});
		} finally {
			resetting = false;
			resetDialogOpen = false;
		}
	}

	function handleViewDetail() {
		goto(`/video/${video.id}`);
	}

	function handleSelectionChange(event: Event) {
		if (onSelectionChange) {
			const checkbox = event.target as HTMLInputElement;
			onSelectionChange(video.id, checkbox.checked);
		}
	}

	// 根据模式确定显示的标题和副标题
	$: displayTitle = customTitle || getEnhancedVideoTitle(video);
	$: displaySubtitle = customSubtitle || video.upper_name;
	$: showUserIcon = mode === 'default';
	$: cardClasses =
		mode === 'default'
			? `group flex h-full min-w-0 flex-col transition-all hover:shadow-md ${selectionMode ? 'cursor-pointer' : ''} ${selected ? 'ring-2 ring-blue-500' : ''}`
			: `transition-all hover:shadow-md ${selectionMode ? 'cursor-pointer' : ''} ${selected ? 'ring-2 ring-blue-500' : ''}`;

	// 从路径中提取番剧名称的通用函数
	function extractBangumiName(path: string): string {
		if (!path) return '';
		const pathParts = path.split(/[/\\]/);
		// 查找最后一个非空的路径部分作为番剧名称
		for (let i = pathParts.length - 1; i >= 0; i--) {
			const part = pathParts[i].trim();
			if (part && part !== '.' && part !== '..') {
				return part;
			}
		}
		return '';
	}

	// 简化的番剧检测逻辑 - 直接使用category字段
	function isBangumiVideo(video: VideoInfo): boolean {
		return video.category === 1;
	}

	// 获取番剧名称用于显示
	function getBangumiName(video: VideoInfo): string {
		if (isBangumiVideo(video)) {
			// 优先使用API获取的真实番剧标题
			if (video.bangumi_title) {
				return video.bangumi_title;
			}
			// 回退到从路径提取
			return extractBangumiName(video.path);
		}
		return '';
	}

	// 获取集数信息用于显示 - 统一处理
	function getEpisodeInfo(video: VideoInfo): string {
		const originalName = video.name.trim();

		// 如果是番剧，尝试美化集数显示
		if (isBangumiVideo(video)) {
			// 如果是纯数字，加上"第X集"
			if (/^\d+$/.test(originalName)) {
				return `第${originalName}集`;
			}
			// 如果已经有"第X话"格式，保持原样
			if (/^第\d+[话集]/.test(originalName)) {
				return originalName;
			}
			// 其他情况直接返回原名
			return originalName;
		}

		return originalName;
	}

	// 统一的视频标题显示逻辑
	function getEnhancedVideoTitle(video: VideoInfo): string {
		// 如果检测到番剧，统一使用两行显示的第二行内容
		if (isBangumiVideo(video)) {
			return getEpisodeInfo(video);
		}

		// 非番剧直接返回原标题
		return video.name.trim();
	}

	// 获取代理后的图片URL
	function getProxiedImageUrl(originalUrl: string): string {
		if (!originalUrl) return '';
		// 使用后端代理端点
		return `/api/proxy/image?url=${encodeURIComponent(originalUrl)}`;
	}
</script>

<Card
	class="{cardClasses} relative overflow-hidden"
	role={selectionMode ? 'button' : undefined}
	tabindex={selectionMode ? 0 : undefined}
	aria-pressed={selectionMode ? selected : undefined}
	onclick={handleCardClick}
	onkeydown={handleCardKeydown}
>
	<!-- 整个卡片的背景模糊图片 -->
	{#if video.cover && mode === 'default'}
		<div
			class="absolute inset-0 scale-110 bg-cover bg-center opacity-20 blur-[2px]"
			style="background-image: url('{getProxiedImageUrl(video.cover)}')"
		></div>
	{/if}

	<!-- 封面图片 -->
	{#if video.cover && mode === 'default'}
		<div class="relative z-10 overflow-hidden rounded-t-lg">
			<!-- 前景清晰图片 -->
			<img
				src={getProxiedImageUrl(video.cover)}
				alt={displayTitle}
				class="aspect-[4/3] w-full object-cover transition-transform duration-200 group-hover:scale-105"
				loading="lazy"
				on:error={(e) => {
					// 封面加载失败时隐藏整个封面容器
					const target = e.currentTarget as HTMLImageElement;
					const container = target.closest('.relative') as HTMLElement;
					if (container) {
						container.style.display = 'none';
					}
				}}
			/>
			<!-- 选择模式复选框覆盖在封面左上角 -->
			{#if selectionMode}
				<div class="absolute top-2 left-2 z-20">
					<input
						type="checkbox"
						checked={selected}
						on:change={handleSelectionChange}
						on:click|stopPropagation
						class="h-5 w-5 rounded border-2 border-white bg-white/80 text-blue-600 shadow-lg backdrop-blur-sm focus:ring-2 focus:ring-blue-500 focus:ring-offset-0"
					/>
				</div>
			{/if}

			<!-- 状态徽章覆盖在封面上 -->
			<div class="absolute top-2 right-2 z-20">
				<Badge variant={overallStatus.color} class="shrink-0 text-xs shadow-md">
					{overallStatus.text}
				</Badge>
			</div>
		</div>
	{/if}

	<CardHeader class="{mode === 'default' ? 'flex-shrink-0 pb-3' : 'pb-3'} relative z-10">
		<div class="flex min-w-0 items-start justify-between gap-2">
			<!-- 选择模式复选框（无封面时显示） -->
			{#if selectionMode && (!video.cover || mode !== 'default')}
				<input
					type="checkbox"
					checked={selected}
					on:change={handleSelectionChange}
					on:click|stopPropagation
					class="mt-0.5 h-4 w-4 shrink-0 rounded border-gray-300 text-blue-600 focus:ring-2 focus:ring-blue-500 focus:ring-offset-0"
				/>
			{/if}
			<CardTitle
				class="line-clamp-2 min-w-0 flex-1 cursor-default text-sm leading-tight"
				title={displayTitle}
			>
				{#if getBangumiName(video)}
					<!-- 两行显示：番剧名 + 集数信息 -->
					<div class="space-y-1">
						<div class="text-primary line-clamp-1 leading-tight font-medium">
							{getBangumiName(video)}
						</div>
						<div class="text-muted-foreground line-clamp-1 text-xs leading-tight">
							{getEpisodeInfo(video)}
						</div>
					</div>
				{:else}
					<!-- 单行显示：原始标题 -->
					<div class="text-primary line-clamp-2 leading-tight font-medium">{displayTitle}</div>
				{/if}
			</CardTitle>
			{#if !video.cover || mode !== 'default'}
				<Badge variant={overallStatus.color} class="shrink-0 text-xs">
					{overallStatus.text}
				</Badge>
			{/if}
		</div>
		{#if displaySubtitle}
			<div class="text-muted-foreground flex min-w-0 items-center gap-1 text-sm">
				{#if showUserIcon}
					<UserIcon class="h-3 w-3 shrink-0" />
				{/if}
				<span class="min-w-0 cursor-default truncate" title={displaySubtitle}>
					{displaySubtitle}
				</span>
			</div>
		{/if}
	</CardHeader>
	<CardContent
		class="{mode === 'default'
			? 'flex min-w-0 flex-1 flex-col justify-end pt-0'
			: 'pt-0'} relative z-10"
	>
		<div class="space-y-3">
			<!-- 进度条区域 -->
			{#if showProgress}
				<div class="space-y-2">
					<div
						class="text-muted-foreground flex justify-between {mode === 'default'
							? 'text-xs'
							: 'text-xs'}"
					>
						<span class="truncate">下载进度</span>
						<span class="shrink-0">{completed}/{total}</span>
					</div>

					<!-- 进度条 -->
					<div class="flex w-full {gap}">
						{#each video.download_status as status, index (index)}
							<Tooltip.Root>
								<Tooltip.Trigger class="flex-1">
									<div
										class="{progressHeight} w-full cursor-help rounded-sm transition-all {getSegmentColor(
											status
										)}"
									></div>
								</Tooltip.Trigger>
								<Tooltip.Content>
									<p>{getTaskName(index)}: {getStatusText(status)}</p>
								</Tooltip.Content>
							</Tooltip.Root>
						{/each}
					</div>
				</div>
			{/if}

			<!-- 操作按钮 -->
			{#if showActions && (mode === 'default' || mode === 'detail')}
				<div class="flex min-w-0 gap-1.5">
					{#if mode === 'default'}
						<Button
							size="sm"
							variant="outline"
							class="min-w-0 flex-1 cursor-pointer px-2 text-xs"
							onclick={handleViewDetail}
						>
							<InfoIcon class="mr-1 h-3 w-3 shrink-0" />
							<span class="truncate">详情</span>
						</Button>
					{/if}
					<Button
						size="sm"
						variant="outline"
						class="{mode === 'detail' ? 'w-full' : 'shrink-0'} cursor-pointer px-2"
						onclick={() => (resetDialogOpen = true)}
					>
						<RotateCcwIcon class="mr-1 h-3 w-3" />
						{mode === 'detail' ? '重置' : ''}
					</Button>
				</div>
			{/if}

			<!-- 路径信息 -->
			{#if video.path && mode === 'detail'}
				<div class="mt-2 space-y-1">
					<div class="text-muted-foreground text-xs">保存路径</div>
					<div class="bg-muted rounded px-2 py-1 font-mono text-xs break-all" title={video.path}>
						{video.path}
					</div>
				</div>
			{/if}
		</div>
	</CardContent>
</Card>

<!-- 重置确认对话框 -->
<AlertDialog.Root bind:open={resetDialogOpen}>
	<AlertDialog.Content>
		<AlertDialog.Header>
			<AlertDialog.Title>确认重置</AlertDialog.Title>
			<AlertDialog.Description>
				<p class="mb-2">
					确定要重置视频 "{displayTitle}" 的下载状态吗？
				</p>
				<p class="text-muted-foreground text-sm">
					• <strong>重置失败</strong>：仅重置失败的任务<br />
					• <strong>强制重置</strong>：重置所有任务，重新下载
				</p>
			</AlertDialog.Description>
		</AlertDialog.Header>
		<AlertDialog.Footer>
			<AlertDialog.Cancel>取消</AlertDialog.Cancel>
			<Button variant="secondary" onclick={() => handleReset(false)} disabled={resetting}>
				{resetting ? '重置中...' : '重置失败'}
			</Button>
			<Button variant="destructive" onclick={() => handleReset(true)} disabled={resetting}>
				{resetting ? '重置中...' : '强制重置'}
			</Button>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
