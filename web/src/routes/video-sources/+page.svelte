<script lang="ts">
	import { onMount } from 'svelte';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Switch } from '$lib/components/ui/switch';
	import { setBreadcrumb } from '$lib/stores/breadcrumb';
	import { toast } from 'svelte-sonner';
	import api from '$lib/api';
	import { VIDEO_SOURCES, type VideoSourceType } from '$lib/consts';
	import { videoSourceStore, setVideoSources } from '$lib/stores/video-source';
	import { runRequest } from '$lib/utils/request.js';
	import { IsMobile } from '$lib/hooks/is-mobile.svelte.js';
	import DeleteVideoSourceDialog from '$lib/components/delete-video-source-dialog.svelte';
	import ResetPathDialog from '$lib/components/reset-path-dialog.svelte';
	import SubmissionSelectionDialog from '$lib/components/submission-selection-dialog.svelte';
	import KeywordFilterDialog from '$lib/components/keyword-filter-dialog.svelte';
	import AiPromptDialog from '$lib/components/ai-prompt-dialog.svelte';
	import AiRenameHistoryDialog from '$lib/components/ai-rename-history-dialog.svelte';

	// 图标导入
	import PlusIcon from '@lucide/svelte/icons/plus';
	import PowerIcon from '@lucide/svelte/icons/power';
	import FolderOpenIcon from '@lucide/svelte/icons/folder-open';
	import TrashIcon from '@lucide/svelte/icons/trash-2';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import ChevronDownIcon from '@lucide/svelte/icons/chevron-down';
	import ChevronRightIcon from '@lucide/svelte/icons/chevron-right';
	import ListVideoIcon from '@lucide/svelte/icons/list-video';
	import FilterIcon from '@lucide/svelte/icons/filter';
	import MusicIcon from '@lucide/svelte/icons/music';
	import FileAudioIcon from '@lucide/svelte/icons/file-audio';
	import FolderSyncIcon from '@lucide/svelte/icons/folder-sync';
	import MessageSquareTextIcon from '@lucide/svelte/icons/message-square-text';
	import SubtitlesIcon from '@lucide/svelte/icons/subtitles';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import SparklesIcon from '@lucide/svelte/icons/sparkles';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import { goto } from '$app/navigation';

	let loading = false;
	let bulkUpdating = false;

	// 响应式相关
	const isMobileQuery = new IsMobile();
	let isMobile: boolean = false;
	// let isTablet: boolean = false; // 未使用，已注释
	$: isMobile = isMobileQuery.current;
	// $: isTablet = innerWidth >= 768 && innerWidth < 1024; // md断点 - 未使用

	// 折叠状态管理 - 默认所有分类都是折叠状态
	let collapsedSections: Record<string, boolean> = {};

	// 批量操作状态（按分类）
	let bulkModeSections: Record<string, boolean> = {};
	let bulkSelectedIds: Record<string, Set<number>> = {};

	// 删除对话框状态
	let showDeleteDialog = false;
	let deleteSourceInfo = {
		type: '',
		id: 0,
		name: ''
	};

	// 路径重设对话框状态
	let showResetPathDialog = false;
	let resetPathSourceInfo = {
		type: '',
		id: 0,
		name: '',
		currentPath: ''
	};

	// 投稿选择对话框状态
	let showSubmissionSelectionDialog = false;
	let submissionSelectionInfo = {
		id: 0,
		upperId: 0,
		upperName: '',
		selectedVideos: [] as string[]
	};

	// 关键词过滤对话框状态
	let showKeywordFilterDialog = false;
	let keywordFilterInfo = {
		type: '',
		id: 0,
		name: ''
	};

	// AI提示词对话框状态
	let showAiPromptDialog = false;
	let aiPromptInfo = {
		type: '',
		id: 0,
		name: '',
		videoPrompt: '',
		audioPrompt: '',
		aiRename: false,
		enableMultiPage: false,
		enableCollection: false,
		enableBangumi: false,
		renameParentDir: false
	};

	// AI批量重命名历史对话框状态
	let showAiRenameHistoryDialog = false;
	let aiRenameHistoryInfo = {
		type: '',
		id: 0,
		name: '',
		videoPrompt: '',
		audioPrompt: '',
		enableMultiPage: false,
		enableCollection: false,
		enableBangumi: false,
		renameParentDir: false
	};

	async function loadVideoSources() {
		const response = await runRequest(() => api.getVideoSources(), {
			setLoading: (value) => (loading = value),
			context: '加载视频源失败'
		});
		if (!response) return;
		setVideoSources(response.data);
	}

	// 投稿源扫描策略配置（分批/自适应）
	let submissionScanBatchSize = '0';
	let submissionAdaptiveScan = false;
	let submissionAdaptiveMaxHours = '24';
	let submissionScanConfigLoading = false;
	let submissionScanConfigSaving = false;

	async function loadSubmissionScanConfig() {
		const response = await runRequest(() => api.getConfig(), {
			setLoading: (value) => (submissionScanConfigLoading = value),
			context: '加载投稿源扫描策略失败'
		});
		if (!response) return;

		const cfg = response.data;
		submissionScanBatchSize = String(cfg.submission_scan_batch_size ?? 0);
		submissionAdaptiveScan = Boolean(cfg.submission_adaptive_scan ?? false);
		submissionAdaptiveMaxHours = String(cfg.submission_adaptive_max_hours ?? 24);
	}

	async function saveSubmissionScanConfig() {
		const batchSize = Number(submissionScanBatchSize);
		const maxHours = Number(submissionAdaptiveMaxHours);

		if (!Number.isFinite(batchSize) || batchSize < 0) {
			toast.error('每轮扫描数量必须是非负数字');
			return;
		}
		if (!Number.isFinite(maxHours) || maxHours < 1) {
			toast.error('最大间隔必须大于等于 1 小时');
			return;
		}

		submissionScanConfigSaving = true;
		const result = await runRequest(
			() =>
				api.updateConfig({
					submission_scan_batch_size: batchSize,
					submission_adaptive_scan: submissionAdaptiveScan,
					submission_adaptive_max_hours: maxHours
				}),
			{ context: '保存投稿源扫描策略失败' }
		);
		submissionScanConfigSaving = false;
		if (!result) return;

		if (result.data.success) {
			toast.success(result.data.message || '保存成功');
		} else {
			toast.error('保存失败', { description: result.data.message });
		}

		await loadSubmissionScanConfig();
	}

	type UpdateResult = { success: boolean; message: string };
	type SuccessToast = { title: string; description?: string };

	async function updateAndReload<T extends UpdateResult>(
		action: () => Promise<{ data: T }>,
		{
			successToast,
			errorTitle = '设置更新失败'
		}: {
			successToast?: (data: T) => SuccessToast;
			errorTitle?: string;
		} = {}
	) {
		const result = await runRequest(action, { context: errorTitle });
		if (!result) return;

		if (!result.data.success) {
			toast.error(errorTitle, { description: result.data.message });
			return;
		}

		const toastInfo = successToast ? successToast(result.data) : { title: result.data.message };
		if (toastInfo.description) {
			toast.success(toastInfo.title, { description: toastInfo.description });
		} else {
			toast.success(toastInfo.title);
		}

		await loadVideoSources();
	}

	function getSelectedSet(sectionKey: string) {
		return bulkSelectedIds[sectionKey] ?? new Set<number>();
	}

	function setSelectedSet(sectionKey: string, set: Set<number>) {
		bulkSelectedIds = { ...bulkSelectedIds, [sectionKey]: set };
	}

	function clearSelection(sectionKey: string) {
		const { [sectionKey]: _removed, ...rest } = bulkSelectedIds;
		bulkSelectedIds = rest;
	}

	function toggleBulkMode(sectionKey: string) {
		const next = !(bulkModeSections[sectionKey] === true);
		bulkModeSections = { ...bulkModeSections, [sectionKey]: next };
		if (!next) {
			clearSelection(sectionKey);
		}
	}

	function toggleSelect(sectionKey: string, sourceId: number) {
		const current = getSelectedSet(sectionKey);
		const next = new Set(current);
		if (next.has(sourceId)) {
			next.delete(sourceId);
		} else {
			next.add(sourceId);
		}
		setSelectedSet(sectionKey, next);
	}

	function selectAll(sectionKey: string, sources: { id: number }[]) {
		setSelectedSet(sectionKey, new Set(sources.map((s) => s.id)));
	}

	function clearAll(sectionKey: string) {
		setSelectedSet(sectionKey, new Set());
	}

	async function bulkSetEnabled(sectionKey: string, sourceType: string, enabled: boolean) {
		const ids = Array.from(getSelectedSet(sectionKey));
		if (ids.length === 0) {
			toast.error('请先选择要操作的视频源');
			return;
		}

		bulkUpdating = true;
		let successCount = 0;
		const failed: { id: number; message: string }[] = [];

		for (const id of ids) {
			const result = await runRequest(() => api.updateVideoSourceEnabled(sourceType, id, enabled), {
				showErrorToast: false,
				onError: (error) => {
					console.error('批量更新失败:', error);
				}
			});

			if (!result) {
				failed.push({ id, message: '请求失败' });
				continue;
			}

			if (result.data.success) {
				successCount += 1;
			} else {
				failed.push({ id, message: result.data.message });
			}
		}

		const actionLabel = enabled ? '启用' : '禁用';
		if (failed.length === 0) {
			toast.success(`批量${actionLabel}成功`, { description: `共 ${successCount} 个视频源` });
		} else {
			const preview = failed
				.slice(0, 3)
				.map((item) => `#${item.id} ${item.message}`)
				.join('；');
			toast.error(`批量${actionLabel}完成（成功 ${successCount}，失败 ${failed.length}）`, {
				description: preview + (failed.length > 3 ? '…' : '')
			});
		}

		await loadVideoSources();
		clearSelection(sectionKey);
		bulkUpdating = false;
	}

	// 切换视频源启用状态
	async function handleToggleEnabled(
		sourceType: string,
		sourceId: number,
		currentEnabled: boolean,
		_sourceName: string // eslint-disable-line @typescript-eslint/no-unused-vars
	) {
		await updateAndReload(
			() => api.updateVideoSourceEnabled(sourceType, sourceId, !currentEnabled),
			{ errorTitle: '操作失败' }
		);
	}

	// 打开删除确认对话框
	function handleDeleteSource(sourceType: string, sourceId: number, sourceName: string) {
		deleteSourceInfo = {
			type: sourceType,
			id: sourceId,
			name: sourceName
		};
		showDeleteDialog = true;
	}

	// 打开路径重设对话框
	function handleResetPath(
		sourceType: string,
		sourceId: number,
		sourceName: string,
		currentPath: string
	) {
		resetPathSourceInfo = {
			type: sourceType,
			id: sourceId,
			name: sourceName,
			currentPath: currentPath
		};
		showResetPathDialog = true;
	}

	// 切换扫描已删除视频设置
	async function handleToggleScanDeleted(
		sourceType: string,
		sourceId: number,
		currentScanDeleted: boolean
	) {
		const newScanDeleted = !currentScanDeleted;
		await updateAndReload(
			() => api.updateVideoSourceScanDeleted(sourceType, sourceId, newScanDeleted),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newScanDeleted ? '已启用扫描已删除视频' : '已禁用扫描已删除视频'
				})
			}
		);
	}

	// 切换仅下载音频设置
	async function handleToggleAudioOnly(
		sourceType: string,
		sourceId: number,
		currentAudioOnly: boolean
	) {
		const newAudioOnly = !currentAudioOnly;
		await updateAndReload(
			() =>
				api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
					audio_only: newAudioOnly
				}),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newAudioOnly ? '已启用仅下载音频模式' : '已禁用仅下载音频模式'
				})
			}
		);
	}

	// 切换仅保留M4A设置
	async function handleToggleAudioOnlyM4aOnly(
		sourceType: string,
		sourceId: number,
		currentAudioOnlyM4aOnly: boolean
	) {
		const newAudioOnlyM4aOnly = !currentAudioOnlyM4aOnly;
		await updateAndReload(
			() =>
				api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
					audio_only_m4a_only: newAudioOnlyM4aOnly
				}),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newAudioOnlyM4aOnly ? '已启用仅保留M4A模式' : '已禁用仅保留M4A模式'
				})
			}
		);
	}

	// 切换平铺目录设置
	async function handleToggleFlatFolder(
		sourceType: string,
		sourceId: number,
		currentFlatFolder: boolean
	) {
		const newFlatFolder = !currentFlatFolder;
		await updateAndReload(
			() =>
				api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
					flat_folder: newFlatFolder
				}),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newFlatFolder ? '已启用平铺目录模式' : '已禁用平铺目录模式'
				})
			}
		);
	}

	// 切换动态API（仅投稿源）
	async function handleToggleDynamicApi(sourceId: number, currentUseDynamicApi: boolean) {
		const newUseDynamicApi = !currentUseDynamicApi;
		await updateAndReload(
			() =>
				api.updateVideoSourceDownloadOptions('submission', sourceId, {
					use_dynamic_api: newUseDynamicApi
				}),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newUseDynamicApi ? '已启用动态API' : '已关闭动态API'
				})
			}
		);
	}

	// 切换下载弹幕设置
	async function handleToggleDownloadDanmaku(
		sourceType: string,
		sourceId: number,
		currentDownloadDanmaku: boolean
	) {
		const newDownloadDanmaku = !currentDownloadDanmaku;
		await updateAndReload(
			() =>
				api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
					download_danmaku: newDownloadDanmaku
				}),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newDownloadDanmaku ? '已启用弹幕下载' : '已禁用弹幕下载'
				})
			}
		);
	}

	// 切换下载字幕设置
	async function handleToggleDownloadSubtitle(
		sourceType: string,
		sourceId: number,
		currentDownloadSubtitle: boolean
	) {
		const newDownloadSubtitle = !currentDownloadSubtitle;
		await updateAndReload(
			() =>
				api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
					download_subtitle: newDownloadSubtitle
				}),
			{
				successToast: () => ({
					title: '设置更新成功',
					description: newDownloadSubtitle ? '已启用字幕下载' : '已禁用字幕下载'
				})
			}
		);
	}

	// 打开AI提示词设置对话框
	function handleOpenAiPromptDialog(
		sourceType: string,
		sourceId: number,
		sourceName: string,
		currentAiRename: boolean,
		videoPrompt: string,
		audioPrompt: string,
		enableMultiPage: boolean,
		enableCollection: boolean,
		enableBangumi: boolean,
		renameParentDir: boolean
	) {
		aiPromptInfo = {
			type: sourceType,
			id: sourceId,
			name: sourceName,
			videoPrompt: videoPrompt || '',
			audioPrompt: audioPrompt || '',
			aiRename: currentAiRename,
			enableMultiPage: enableMultiPage || false,
			enableCollection: enableCollection || false,
			enableBangumi: enableBangumi || false,
			renameParentDir: renameParentDir || false
		};
		showAiPromptDialog = true;
	}

	// AI提示词保存后的回调
	async function handleAiPromptSave() {
		await loadVideoSources();
	}

	// AI批量重命名历史文件 - 打开对话框
	function handleAiRenameHistory(
		sourceType: string,
		sourceId: number,
		sourceName: string,
		videoPrompt: string,
		audioPrompt: string,
		enableMultiPage: boolean,
		enableCollection: boolean,
		enableBangumi: boolean,
		renameParentDir: boolean
	) {
		aiRenameHistoryInfo = {
			type: sourceType,
			id: sourceId,
			name: sourceName,
			videoPrompt: videoPrompt || '',
			audioPrompt: audioPrompt || '',
			enableMultiPage: enableMultiPage || false,
			enableCollection: enableCollection || false,
			enableBangumi: enableBangumi || false,
			renameParentDir: renameParentDir || false
		};
		showAiRenameHistoryDialog = true;
	}

	// AI批量重命名完成后的回调
	function handleAiRenameHistoryComplete() {
		// 刷新视频源列表以显示最新状态（AI重命名已开启）
		loadVideoSources();
	}

	// 确认删除
	async function handleConfirmDelete(event: CustomEvent<{ deleteLocalFiles: boolean }>) {
		const { deleteLocalFiles } = event.detail;

		await updateAndReload(
			() => api.deleteVideoSource(deleteSourceInfo.type, deleteSourceInfo.id, deleteLocalFiles),
			{
				errorTitle: '删除失败',
				successToast: (data) => ({
					title: '删除成功',
					description: data.message + (deleteLocalFiles ? '，本地文件已删除' : '，本地文件已保留')
				})
			}
		);
	}

	// 取消删除
	function handleCancelDelete() {
		showDeleteDialog = false;
	}

	// 确认路径重设
	async function handleConfirmResetPath(
		event: CustomEvent<{
			new_path: string;
			apply_rename_rules?: boolean;
			clean_empty_folders?: boolean;
		}>
	) {
		const request = event.detail;

		await updateAndReload(
			() => api.resetVideoSourcePath(resetPathSourceInfo.type, resetPathSourceInfo.id, request),
			{
				errorTitle: '路径重设失败',
				successToast: (data) => ({
					title: '路径重设成功',
					description:
						data.message +
						(request.apply_rename_rules ? `，已移动 ${data.moved_files_count} 个文件` : '')
				})
			}
		);
	}

	// 取消路径重设
	function handleCancelResetPath() {
		showResetPathDialog = false;
	}

	// 打开投稿选择对话框
	function handleSelectSubmissionVideos(
		sourceId: number,
		upperId: number | undefined,
		upperName: string,
		selectedVideosJson: string | null | undefined
	) {
		if (!upperId) {
			toast.error('无法选择历史投稿', { description: '缺少 UP 主 ID' });
			return;
		}

		let selectedVideos: string[] = [];
		if (selectedVideosJson) {
			try {
				selectedVideos = JSON.parse(selectedVideosJson);
			} catch (e) {
				console.error('解析选中视频列表失败:', e);
			}
		}
		submissionSelectionInfo = {
			id: sourceId,
			upperId,
			upperName,
			selectedVideos
		};
		showSubmissionSelectionDialog = true;
	}

	// 确认投稿选择
	async function handleConfirmSubmissionSelection(event: CustomEvent<string[]>) {
		const selectedVideos = event.detail;
		try {
			const result = await api.updateSubmissionSelectedVideos(
				submissionSelectionInfo.id,
				selectedVideos
			);
			if (result.data.success) {
				toast.success('历史投稿选择已更新', {
					description: result.data.message
				});
				await loadVideoSources();
			} else {
				toast.error('更新失败', { description: result.data.message });
			}
		} catch (error: unknown) {
			console.error('更新投稿选择失败:', error);
			toast.error('更新失败', { description: (error as Error).message });
		}
	}

	// 取消投稿选择
	function handleCancelSubmissionSelection() {
		showSubmissionSelectionDialog = false;
	}

	// 打开关键词过滤对话框
	function handleOpenKeywordFilter(sourceType: string, sourceId: number, sourceName: string) {
		keywordFilterInfo = {
			type: sourceType,
			id: sourceId,
			name: sourceName
		};
		showKeywordFilterDialog = true;
	}

	// 关键词保存成功
	function handleKeywordFilterSave() {
		toast.success('关键词过滤器已更新');
		loadVideoSources();
	}

	// 取消关键词过滤
	function handleKeywordFilterCancel() {
		showKeywordFilterDialog = false;
	}

	// 切换折叠状态
	function toggleCollapse(sectionKey: string) {
		// 如果未设置，默认为折叠状态(true)，点击后变为展开状态(false)
		// 如果已设置，则切换状态
		if (collapsedSections[sectionKey] === undefined) {
			collapsedSections[sectionKey] = false; // 第一次点击展开
		} else {
			collapsedSections[sectionKey] = !collapsedSections[sectionKey];
		}
		collapsedSections = { ...collapsedSections };

		// 折叠时退出批量模式，避免误操作
		if (collapsedSections[sectionKey] !== false) {
			bulkModeSections = { ...bulkModeSections, [sectionKey]: false };
			clearSelection(sectionKey);
		}
	}

	function navigateToAddSource() {
		goto('/add-source');
	}

	onMount(() => {
		setBreadcrumb([{ label: '视频源管理' }]);
		loadVideoSources();
		loadSubmissionScanConfig();
	});
</script>

<svelte:head>
	<title>视频源管理 - Bili Sync</title>
</svelte:head>

<div class="space-y-6">
	<!-- 页面头部 -->
	<div class="flex {isMobile ? 'flex-col gap-4' : 'flex-row items-center justify-between gap-4'}">
		<div>
			<h1 class="{isMobile ? 'text-xl' : 'text-2xl'} font-bold">视频源管理</h1>
			<p class="{isMobile ? 'text-sm' : 'text-base'} text-muted-foreground">
				管理和配置您的视频源，包括收藏夹、合集、投稿和稍后再看
			</p>
		</div>
		<Button
			onclick={navigateToAddSource}
			class="flex items-center gap-2 {isMobile ? 'w-full' : 'w-auto'}"
		>
			<PlusIcon class="h-4 w-4" />
			添加视频源
		</Button>
	</div>

	{#if loading}
		<div class="flex items-center justify-center py-12">
			<div class="text-muted-foreground">加载中...</div>
		</div>
	{:else}
		<!-- 视频源分类展示 -->
		<div class="grid gap-6">
			{#each Object.entries(VIDEO_SOURCES) as [sourceKey, sourceConfig] (sourceKey)}
				{@const sources = $videoSourceStore
					? $videoSourceStore[sourceConfig.type as VideoSourceType]
					: []}
				<Card>
					<CardHeader class="cursor-pointer" onclick={() => toggleCollapse(sourceKey)}>
						<CardTitle class="flex items-center gap-2">
							{#if collapsedSections[sourceKey] !== false}
								<ChevronRightIcon class="text-muted-foreground h-4 w-4" />
							{:else}
								<ChevronDownIcon class="text-muted-foreground h-4 w-4" />
							{/if}
							<sourceConfig.icon class="h-5 w-5" />
							{sourceConfig.title}
							<Badge variant="outline" class="ml-auto">
								{sources?.length || 0} 个
							</Badge>
						</CardTitle>
					</CardHeader>
					{#if collapsedSections[sourceKey] === false}
						<CardContent>
							{#if sourceKey === 'SUBMISSION'}
								<div class="mb-4 rounded-lg border p-4">
									<div class="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
										<div class="space-y-1">
											<div class="text-sm font-semibold">投稿源扫描优化</div>
											<div class="text-muted-foreground text-xs">
												分批扫描可以降低单轮请求量；自适应会对长期无更新的UP自动降频，减少风控概率。
											</div>
										</div>

										<div class="flex items-center gap-2 md:justify-end">
											<Button
												size="sm"
												variant="outline"
												disabled={submissionScanConfigLoading || submissionScanConfigSaving}
												onclick={loadSubmissionScanConfig}
											>
												刷新
											</Button>
											<Button
												size="sm"
												disabled={submissionScanConfigLoading || submissionScanConfigSaving}
												onclick={saveSubmissionScanConfig}
											>
												{submissionScanConfigSaving ? '保存中…' : '保存'}
											</Button>
										</div>
									</div>

									<div class="mt-4 grid gap-4 md:grid-cols-3">
										<div class="space-y-1">
											<Label>每轮扫描数量</Label>
											<Input
												type="number"
												min="0"
												step="1"
												disabled={submissionScanConfigLoading || submissionScanConfigSaving}
												bind:value={submissionScanBatchSize}
											/>
											<div class="text-muted-foreground text-xs">0 表示不限制（保持旧行为），建议 30~80。</div>
										</div>

										<div class="flex items-center gap-3 md:pt-7">
											<Switch
												disabled={submissionScanConfigLoading || submissionScanConfigSaving}
												bind:checked={submissionAdaptiveScan}
											/>
											<div class="space-y-0.5">
												<Label>自适应扫描频率</Label>
												<div class="text-muted-foreground text-xs">连续无更新会逐步延长扫描间隔。</div>
											</div>
										</div>

										<div class="space-y-1">
											<Label>最大间隔（小时）</Label>
											<Input
												type="number"
												min="1"
												max="168"
												step="1"
												disabled={!submissionAdaptiveScan || submissionScanConfigLoading || submissionScanConfigSaving}
												bind:value={submissionAdaptiveMaxHours}
											/>
											<div class="text-muted-foreground text-xs">范围 1~168。</div>
										</div>
									</div>
								</div>
							{/if}

							{#if sources && sources.length > 0}
								{@const bulkMode = bulkModeSections[sourceKey] === true}
								{@const selectedSet = bulkSelectedIds[sourceKey] ?? new Set<number>()}

								<div
									class="mb-3 flex flex-wrap items-center gap-2 {isMobile ? 'justify-between' : ''}"
								>
									<Button
										size="sm"
										variant="outline"
										disabled={bulkUpdating}
										onclick={() => toggleBulkMode(sourceKey)}
									>
										{bulkMode ? '退出批量' : '批量操作'}
									</Button>

									{#if bulkMode}
										<span class="text-muted-foreground text-sm">
											已选 {selectedSet.size} / {sources.length}
										</span>

										<div class="ml-auto flex flex-wrap items-center gap-2">
											<Button
												size="sm"
												variant="outline"
												disabled={bulkUpdating}
												onclick={() => selectAll(sourceKey, sources)}
											>
												全选
											</Button>
											<Button
												size="sm"
												variant="outline"
												disabled={bulkUpdating || selectedSet.size === 0}
												onclick={() => clearAll(sourceKey)}
											>
												清空
											</Button>
											<Button
												size="sm"
												disabled={bulkUpdating || selectedSet.size === 0}
												onclick={() => bulkSetEnabled(sourceKey, sourceConfig.type, true)}
											>
												批量启用
											</Button>
											<Button
												size="sm"
												variant="secondary"
												disabled={bulkUpdating || selectedSet.size === 0}
												onclick={() => bulkSetEnabled(sourceKey, sourceConfig.type, false)}
											>
												批量禁用
											</Button>
										</div>
									{/if}
								</div>

								<div class="space-y-3">
									{#each sources as source (source.id)}
										<div
											class="flex {isMobile
												? 'flex-col gap-3'
												: 'flex-row items-center justify-between gap-3'} rounded-lg border p-3"
										>
											{#if bulkMode}
												<label class="flex items-center {isMobile ? 'self-start' : ''}">
													<input
														type="checkbox"
														checked={selectedSet.has(source.id)}
														disabled={bulkUpdating}
														onchange={() => toggleSelect(sourceKey, source.id)}
														class="h-4 w-4 rounded border-gray-300"
													/>
												</label>
											{/if}
											<div class="min-w-0 flex-1">
												<div
													class="flex {isMobile
														? 'flex-col gap-2'
														: 'flex-row items-center gap-2'} mb-1"
												>
													<span class="truncate font-medium">{source.name}</span>
													<Badge
														variant={source.enabled ? 'default' : 'secondary'}
														class="w-fit text-xs"
													>
														{source.enabled ? '已启用' : '已禁用'}
													</Badge>
												</div>
												<div class="text-muted-foreground truncate text-sm" title={source.path}>
													{source.path || '未设置路径'}
												</div>
												<!-- 显示对应类型的ID -->
												<div class="text-muted-foreground mt-1 text-xs">
													{#if sourceConfig.type === 'favorite' && source.f_id}
														收藏夹ID: {source.f_id}
													{:else if sourceConfig.type === 'collection' && source.s_id}
														合集ID: {source.s_id}
														{#if source.m_id}
															| UP主ID: {source.m_id}{/if}
													{:else if sourceConfig.type === 'submission' && source.upper_id}
														UP主ID: {source.upper_id}
														{#if source.selected_videos}
															{@const selectedCount = (() => {
																try {
																	return JSON.parse(source.selected_videos).length;
																} catch {
																	return 0;
																}
															})()}
															{#if selectedCount > 0}
																<span class="ml-2 text-purple-600"
																	>| 已选 {selectedCount} 个历史投稿</span
																>
															{/if}
														{/if}
													{:else if sourceConfig.type === 'bangumi'}
														{#if source.season_id}<span class="block"
																>主季度ID: {source.season_id}</span
															>{/if}
														{#if source.selected_seasons?.length}
															<span class="block"
																>已选季度ID: {source.selected_seasons.join(', ')}</span
															>
														{/if}
														{#if source.media_id}<span class="block"
																>Media ID: {source.media_id}</span
															>{/if}
													{:else if sourceConfig.type === 'watch_later'}
														稍后再看 (无特定ID)
													{/if}
												</div>
												{#if source.scan_deleted_videos}
													<div class="mt-1 text-xs text-blue-600">扫描删除视频已启用</div>
												{/if}
												{#if source.keyword_filters && source.keyword_filters.length > 0}
													<div class="mt-1 text-xs text-purple-600">
														已配置 {source.keyword_filters.length} 个关键词过滤器
													</div>
												{/if}
												<!-- 下载选项状态显示 -->
												<div class="mt-1 flex flex-wrap gap-2 text-xs">
													{#if source.audio_only}
														<span class="text-amber-600">仅音频模式</span>
														{#if source.audio_only_m4a_only}
															<span class="text-amber-500">仅M4A</span>
														{/if}
													{/if}
													{#if source.flat_folder}
														<span class="text-purple-600">平铺目录</span>
													{/if}
													{#if source.use_dynamic_api}
														<span class="text-blue-600">动态API已启用</span>
													{/if}
													{#if source.download_danmaku === false}
														<span class="text-gray-500">弹幕下载已禁用</span>
													{/if}
													{#if source.download_subtitle === false}
														<span class="text-gray-500">字幕下载已禁用</span>
													{/if}
													{#if source.ai_rename}
														<span class="text-blue-600">
															AI重命名已启用{#if source.ai_rename_video_prompt || source.ai_rename_audio_prompt}（自定义提示词）{/if}
														</span>
													{/if}
												</div>
											</div>

											<div class="flex items-center justify-end gap-1 sm:ml-4">
												<!-- 启用/禁用 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleToggleEnabled(
															sourceConfig.type,
															source.id,
															source.enabled,
															source.name
														)}
													title={source.enabled ? '禁用' : '启用'}
													class="h-8 w-8 p-0"
												>
													<PowerIcon
														class="h-4 w-4 {source.enabled ? 'text-green-600' : 'text-gray-400'}"
													/>
												</Button>

												<!-- 选择历史投稿（仅投稿类型显示） -->
												{#if sourceConfig.type === 'submission'}
													<Button
														size="sm"
														variant="ghost"
														onclick={() =>
															handleSelectSubmissionVideos(
																source.id,
																source.upper_id,
																source.name,
																source.selected_videos
															)}
														title="选择历史投稿"
														class="h-8 w-8 p-0"
													>
														<ListVideoIcon class="h-4 w-4 text-purple-600" />
													</Button>

													<Button
														size="sm"
														variant="ghost"
														onclick={() =>
															handleToggleDynamicApi(source.id, source.use_dynamic_api ?? false)}
														title="只有使用动态API才能拉取到动态视频，但该接口不提供分页参数，每次请求只能拉取12条视频。这会一定程度上增加请求次数，用户可根据实际情况酌情选择，推荐仅在UP主有较多动态视频时开启。"
														class="h-8 w-8 p-0"
													>
														<ActivityIcon
															class="h-4 w-4 {source.use_dynamic_api ? 'text-blue-600' : 'text-gray-400'}"
														/>
													</Button>
												{/if}

												<!-- 重设路径 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleResetPath(sourceConfig.type, source.id, source.name, source.path)}
													title="重设路径"
													class="h-8 w-8 p-0"
												>
													<FolderOpenIcon class="h-4 w-4 text-orange-600" />
												</Button>

												<!-- 扫描删除视频设置 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleToggleScanDeleted(
															sourceConfig.type,
															source.id,
															source.scan_deleted_videos
														)}
													title={source.scan_deleted_videos ? '禁用扫描已删除' : '启用扫描已删除'}
													class="h-8 w-8 p-0"
												>
													<RotateCcwIcon
														class="h-4 w-4 {source.scan_deleted_videos
															? 'text-blue-600'
															: 'text-gray-400'}"
													/>
												</Button>

												<!-- 关键词过滤 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleOpenKeywordFilter(sourceConfig.type, source.id, source.name)}
													title="关键词过滤"
													class="h-8 w-8 p-0"
												>
													<FilterIcon
														class="h-4 w-4 {source.keyword_filters &&
														source.keyword_filters.length > 0
															? 'text-purple-600'
															: 'text-gray-400'}"
													/>
												</Button>

												<!-- 仅下载音频 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleToggleAudioOnly(
															sourceConfig.type,
															source.id,
															source.audio_only ?? false
														)}
													title={source.audio_only ? '禁用仅音频模式' : '启用仅音频模式'}
													class="h-8 w-8 p-0"
												>
													<MusicIcon
														class="h-4 w-4 {source.audio_only ? 'text-amber-600' : 'text-gray-400'}"
													/>
												</Button>

												<!-- 仅保留M4A（仅在音频模式开启时显示） -->
												{#if source.audio_only}
													<Button
														size="sm"
														variant="ghost"
														onclick={() =>
															handleToggleAudioOnlyM4aOnly(
																sourceConfig.type,
																source.id,
																source.audio_only_m4a_only ?? false
															)}
														title={source.audio_only_m4a_only ? '禁用仅M4A模式' : '启用仅M4A模式'}
														class="h-8 w-8 p-0"
													>
														<FileAudioIcon
															class="h-4 w-4 {source.audio_only_m4a_only
																? 'text-amber-500'
																: 'text-gray-400'}"
														/>
													</Button>
												{/if}

												<!-- 平铺目录 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleToggleFlatFolder(
															sourceConfig.type,
															source.id,
															source.flat_folder ?? false
														)}
													title={source.flat_folder ? '禁用平铺目录' : '启用平铺目录'}
													class="h-8 w-8 p-0"
												>
													<FolderSyncIcon
														class="h-4 w-4 {source.flat_folder
															? 'text-purple-600'
															: 'text-gray-400'}"
													/>
												</Button>

												<!-- 下载弹幕 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleToggleDownloadDanmaku(
															sourceConfig.type,
															source.id,
															source.download_danmaku ?? true
														)}
													title={source.download_danmaku !== false
														? '禁用弹幕下载'
														: '启用弹幕下载'}
													class="h-8 w-8 p-0"
												>
													<MessageSquareTextIcon
														class="h-4 w-4 {source.download_danmaku !== false
															? 'text-green-600'
															: 'text-gray-400'}"
													/>
												</Button>

												<!-- 下载字幕 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleToggleDownloadSubtitle(
															sourceConfig.type,
															source.id,
															source.download_subtitle ?? true
														)}
													title={source.download_subtitle !== false
														? '禁用字幕下载'
														: '启用字幕下载'}
													class="h-8 w-8 p-0"
												>
													<SubtitlesIcon
														class="h-4 w-4 {source.download_subtitle !== false
															? 'text-green-600'
															: 'text-gray-400'}"
													/>
												</Button>

												<!-- AI重命名 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleOpenAiPromptDialog(
															sourceConfig.type,
															source.id,
															source.name,
															source.ai_rename ?? false,
															source.ai_rename_video_prompt ?? '',
															source.ai_rename_audio_prompt ?? '',
															source.ai_rename_enable_multi_page ?? false,
															source.ai_rename_enable_collection ?? false,
															source.ai_rename_enable_bangumi ?? false,
															source.ai_rename_rename_parent_dir ?? false
														)}
													title="AI重命名设置"
													class="h-8 w-8 p-0"
												>
													<SparklesIcon
														class="h-4 w-4 {source.ai_rename ? 'text-blue-600' : 'text-gray-400'}"
													/>
												</Button>

												<!-- AI批量重命名历史 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleAiRenameHistory(
															sourceConfig.type,
															source.id,
															source.name,
															source.ai_rename_video_prompt ?? '',
															source.ai_rename_audio_prompt ?? '',
															source.ai_rename_enable_multi_page ?? false,
															source.ai_rename_enable_collection ?? false,
															source.ai_rename_enable_bangumi ?? false,
															source.ai_rename_rename_parent_dir ?? false
														)}
													title="AI批量重命名历史文件"
													class="h-8 w-8 p-0"
												>
													<HistoryIcon
														class="h-4 w-4 {source.ai_rename ? 'text-cyan-600' : 'text-gray-400'}"
													/>
												</Button>

												<!-- 删除 -->
												<Button
													size="sm"
													variant="ghost"
													onclick={() =>
														handleDeleteSource(sourceConfig.type, source.id, source.name)}
													title="删除"
													class="h-8 w-8 p-0"
												>
													<TrashIcon class="text-destructive h-4 w-4" />
												</Button>
											</div>
										</div>
									{/each}
								</div>
							{:else}
								<div class="flex flex-col items-center justify-center py-8 text-center">
									<sourceConfig.icon class="text-muted-foreground mb-4 h-12 w-12" />
									<div class="text-muted-foreground mb-2">暂无{sourceConfig.title}</div>
									<p class="text-muted-foreground mb-4 text-sm">
										{#if sourceConfig.type === 'favorite'}
											还没有添加任何收藏夹订阅
										{:else if sourceConfig.type === 'collection'}
											还没有添加任何合集或列表订阅
										{:else if sourceConfig.type === 'submission'}
											还没有添加任何用户投稿订阅
										{:else}
											还没有添加稍后再看订阅
										{/if}
									</p>
									<Button size="sm" variant="outline" onclick={navigateToAddSource}>
										<PlusIcon class="mr-2 h-4 w-4" />
										添加{sourceConfig.title}
									</Button>
								</div>
							{/if}
						</CardContent>
					{/if}
				</Card>
			{/each}
		</div>
	{/if}
</div>

<!-- 删除确认对话框 -->
<DeleteVideoSourceDialog
	bind:isOpen={showDeleteDialog}
	sourceName={deleteSourceInfo.name}
	sourceType={deleteSourceInfo.type}
	on:confirm={handleConfirmDelete}
	on:cancel={handleCancelDelete}
/>

<!-- 路径重设对话框 -->
<ResetPathDialog
	bind:isOpen={showResetPathDialog}
	sourceName={resetPathSourceInfo.name}
	sourceType={resetPathSourceInfo.type}
	currentPath={resetPathSourceInfo.currentPath}
	on:confirm={handleConfirmResetPath}
	on:cancel={handleCancelResetPath}
/>

<!-- 投稿选择对话框 -->
<SubmissionSelectionDialog
	bind:isOpen={showSubmissionSelectionDialog}
	sourceId={submissionSelectionInfo.id}
	upperId={submissionSelectionInfo.upperId}
	upperName={submissionSelectionInfo.upperName}
	initialSelectedVideos={submissionSelectionInfo.selectedVideos}
	on:confirm={handleConfirmSubmissionSelection}
	on:cancel={handleCancelSubmissionSelection}
/>

<!-- 关键词过滤对话框 -->
<KeywordFilterDialog
	bind:isOpen={showKeywordFilterDialog}
	sourceName={keywordFilterInfo.name}
	sourceType={keywordFilterInfo.type}
	sourceId={keywordFilterInfo.id}
	on:save={handleKeywordFilterSave}
	on:cancel={handleKeywordFilterCancel}
/>

<!-- AI提示词设置对话框 -->
<AiPromptDialog
	bind:isOpen={showAiPromptDialog}
	sourceName={aiPromptInfo.name}
	sourceType={aiPromptInfo.type}
	sourceId={aiPromptInfo.id}
	initialVideoPrompt={aiPromptInfo.videoPrompt}
	initialAudioPrompt={aiPromptInfo.audioPrompt}
	initialAiRename={aiPromptInfo.aiRename}
	initialEnableMultiPage={aiPromptInfo.enableMultiPage}
	initialEnableCollection={aiPromptInfo.enableCollection}
	initialEnableBangumi={aiPromptInfo.enableBangumi}
	initialRenameParentDir={aiPromptInfo.renameParentDir}
	on:save={handleAiPromptSave}
/>

<!-- AI批量重命名历史对话框 -->
<AiRenameHistoryDialog
	bind:isOpen={showAiRenameHistoryDialog}
	sourceName={aiRenameHistoryInfo.name}
	sourceType={aiRenameHistoryInfo.type}
	sourceId={aiRenameHistoryInfo.id}
	initialVideoPrompt={aiRenameHistoryInfo.videoPrompt}
	initialAudioPrompt={aiRenameHistoryInfo.audioPrompt}
	initialEnableMultiPage={aiRenameHistoryInfo.enableMultiPage}
	initialEnableCollection={aiRenameHistoryInfo.enableCollection}
	initialEnableBangumi={aiRenameHistoryInfo.enableBangumi}
	initialRenameParentDir={aiRenameHistoryInfo.renameParentDir}
	on:complete={handleAiRenameHistoryComplete}
/>
