<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { createEventDispatcher } from 'svelte';
	import { api } from '$lib/api';
	import { toast } from 'svelte-sonner';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Label } from '$lib/components/ui/label';
	import { Switch } from '$lib/components/ui/switch';
	import { Button } from '$lib/components/ui/button';

	export let isOpen = false;
	export let sourceName = '';
	export let sourceType = '';
	export let sourceId = 0;
	export let initialVideoPrompt = '';
	export let initialAudioPrompt = '';
export let initialAiRename = false;
// 高级选项初始值
export let initialEnableMultiPage = false;
export let initialEnableCollection = false;
export let initialEnableBangumi = false;
export let initialRenameParentDir = false;

	const dispatch = createEventDispatcher<{
		save: {
			videoPrompt: string;
			audioPrompt: string;
			aiRename: boolean;
			enableMultiPage: boolean;
			enableCollection: boolean;
			enableBangumi: boolean;
			renameParentDir: boolean;
		};
		cancel: void;
	}>();

	let videoPrompt = '';
	let audioPrompt = '';
	let aiRename = false;
	let isSaving = false;
	let isClearing = false;

	// 高级选项
let showAdvancedOptions = false;
let enableMultiPage = false;
let enableCollection = false;
let enableBangumi = false;
let renameParentDir = false;

	// 重置状态
	function resetState() {
		videoPrompt = initialVideoPrompt;
		audioPrompt = initialAudioPrompt;
	aiRename = initialAiRename;
	enableMultiPage = initialEnableMultiPage;
	enableCollection = initialEnableCollection;
	enableBangumi = initialEnableBangumi;
	renameParentDir = initialRenameParentDir;
	showAdvancedOptions =
		initialEnableMultiPage || initialEnableCollection || initialEnableBangumi || initialRenameParentDir;
	isSaving = false;
	isClearing = false;
}

	// 当对话框打开时重置状态
	$: if (isOpen) {
		resetState();
	}

	// 获取视频源类型的中文名称
	function getSourceTypeLabel(type: string): string {
		const typeMap: Record<string, string> = {
			collection: '合集',
			favorite: '收藏夹',
			submission: 'UP主投稿',
			watch_later: '稍后观看',
			bangumi: '番剧'
		};
		return typeMap[type] || type;
	}

	// 清除该源的AI对话缓存
	async function handleClearCache() {
		isClearing = true;
		try {
			const result = await api.clearAiRenameCacheForSource(sourceType, sourceId);
			if (result.data.success) {
				toast.success('已清除该源的AI对话历史');
			} else {
				toast.error('清除失败', { description: result.data.message });
			}
		} catch (error) {
			console.error('清除AI缓存失败:', error);
			toast.error('清除失败', { description: (error as Error).message });
		} finally {
			isClearing = false;
		}
	}

	// 保存设置
	async function handleSave() {
		isSaving = true;
		try {
			const result = await api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
				ai_rename: aiRename,
				ai_rename_video_prompt: videoPrompt.trim(),
				ai_rename_audio_prompt: audioPrompt.trim(),
				ai_rename_enable_multi_page: enableMultiPage,
				ai_rename_enable_collection: enableCollection,
				ai_rename_enable_bangumi: enableBangumi,
				ai_rename_rename_parent_dir: renameParentDir
			});

			if (result.data.success) {
				toast.success('AI重命名设置已保存');
				dispatch('save', {
					videoPrompt: videoPrompt.trim(),
					audioPrompt: audioPrompt.trim(),
					aiRename,
					enableMultiPage,
					enableCollection,
					enableBangumi,
					renameParentDir
				});
				isOpen = false;
			} else {
				toast.error('保存失败', { description: result.data.message });
			}
		} catch (error) {
			console.error('保存AI提示词设置失败:', error);
			toast.error('保存失败', { description: (error as Error).message });
		} finally {
			isSaving = false;
		}
	}

	function handleCancel() {
		dispatch('cancel');
		isOpen = false;
	}
</script>

<AlertDialog.Root bind:open={isOpen}>
	<AlertDialog.Content class="max-w-lg">
		<AlertDialog.Header>
			<AlertDialog.Title title={sourceName ? `AI重命名设置 - ${sourceName}` : 'AI重命名设置'}>
				AI重命名设置
			</AlertDialog.Title>
			<AlertDialog.Description>
				为 {getSourceTypeLabel(sourceType)} 「{sourceName}」 设置自定义AI重命名提示词
			</AlertDialog.Description>
		</AlertDialog.Header>

		<div class="space-y-4 py-4">
			<!-- 启用/禁用开关 -->
			<div class="flex items-center justify-between rounded-lg border p-3">
				<div>
					<Label class="text-sm font-medium">启用AI重命名</Label>
					<p class="text-muted-foreground text-xs">使用AI对下载的文件进行智能重命名</p>
				</div>
				<Switch bind:checked={aiRename} />
			</div>

			{#if aiRename}
				<!-- 视频提示词 -->
				<div class="space-y-2">
					<Label for="video-prompt">视频重命名提示词</Label>
					<Textarea
						id="video-prompt"
						bind:value={videoPrompt}
						placeholder="留空则使用全局配置的提示词..."
						rows={3}
						class="resize-none"
					/>
					<p class="text-muted-foreground text-xs">
						针对视频文件的AI重命名提示词，留空将使用全局配置
					</p>
				</div>

				<!-- 音频提示词 -->
				<div class="space-y-2">
					<Label for="audio-prompt">音频重命名提示词</Label>
					<Textarea
						id="audio-prompt"
						bind:value={audioPrompt}
						placeholder="留空则使用全局配置的提示词..."
						rows={3}
						class="resize-none"
					/>
					<p class="text-muted-foreground text-xs">
						针对音频文件的AI重命名提示词，留空将使用全局配置
					</p>
				</div>

				<!-- 提示词写法说明 -->
				<div
					class="rounded-lg border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950"
				>
					<p class="text-xs text-amber-700 dark:text-amber-300">
						<strong>⚠️ 注意：</strong
						>提示词需具体明确，模糊的描述（如"作者"）可能被理解为UP主而非歌手。<br />
						<strong>💡 写法：</strong>AI会严格按格式生成，不添加额外信息。<br />
						<span class="font-mono">示例：BV号-歌手名-日期</span
						>（歌手从标题《》前提取，日期用YYYYMMDD）<br />
						可用字段：BV号、UP主、标题、歌手、分区、日期、排序位置等
					</p>
				</div>

				<!-- 高级选项（默认关闭） -->
				<div
					class="space-y-3 rounded-lg border border-gray-200 bg-gray-50 p-3 dark:border-gray-700 dark:bg-gray-900"
				>
					<button
						type="button"
						onclick={() => (showAdvancedOptions = !showAdvancedOptions)}
						class="flex w-full items-center justify-between text-left"
					>
						<span class="text-sm font-medium text-gray-700 dark:text-gray-300"
							>高级选项（默认关闭，有风险）</span
						>
						<svg
							class="h-4 w-4 transform text-gray-500 transition-transform {showAdvancedOptions
								? 'rotate-180'
								: ''}"
							fill="none"
							stroke="currentColor"
							viewBox="0 0 24 24"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								stroke-width="2"
								d="M19 9l-7 7-7-7"
							/>
						</svg>
					</button>

					{#if showAdvancedOptions}
						<div class="space-y-2 pt-2">
							<div class="flex items-center space-x-2">
								<input
									type="checkbox"
									id="enable-multi-page-prompt"
									bind:checked={enableMultiPage}
									class="border-input h-4 w-4 rounded border"
								/>
								<Label for="enable-multi-page-prompt" class="text-sm leading-none font-medium">
									对多P视频启用AI重命名
								</Label>
							</div>
							<div class="flex items-center space-x-2">
								<input
									type="checkbox"
									id="enable-collection-prompt"
									bind:checked={enableCollection}
									class="border-input h-4 w-4 rounded border"
								/>
								<Label for="enable-collection-prompt" class="text-sm leading-none font-medium">
									对合集视频启用AI重命名
								</Label>
							</div>
							<div class="flex items-center space-x-2">
								<input
									type="checkbox"
									id="enable-bangumi-prompt"
									bind:checked={enableBangumi}
									class="border-input h-4 w-4 rounded border"
								/>
								<Label for="enable-bangumi-prompt" class="text-sm leading-none font-medium">
									对番剧启用AI重命名
								</Label>
							</div>
							<div class="flex items-center space-x-2">
								<input
									type="checkbox"
									id="rename-parent-dir-prompt"
									bind:checked={renameParentDir}
									class="border-input h-4 w-4 rounded border"
								/>
								<Label for="rename-parent-dir-prompt" class="text-sm leading-none font-medium">
									重命名上级目录
								</Label>
							</div>
							<!-- 风险警告 -->
							<div
								class="rounded border border-red-200 bg-red-50 p-2 dark:border-red-800 dark:bg-red-950"
							>
								<p class="text-xs text-red-700 dark:text-red-300">
									<strong>⚠️ 风险警告：</strong
									>以上选项为实验性功能，可能存在命名Bug导致视频文件丢失或无法识别。
									启用后果自负，建议先在小范围测试。
								</p>
							</div>
						</div>
					{/if}
				</div>
			{/if}
		</div>

		<AlertDialog.Footer class="flex justify-between sm:justify-between">
			<Button
				variant="outline"
				size="sm"
				onclick={handleClearCache}
				disabled={isClearing}
				class="text-orange-600 hover:text-orange-700 dark:text-orange-400"
			>
				{isClearing ? '清除中...' : '清除对话缓存'}
			</Button>
			<div class="flex gap-2">
				<AlertDialog.Cancel onclick={handleCancel}>取消</AlertDialog.Cancel>
				<AlertDialog.Action onclick={handleSave} disabled={isSaving}>
					{isSaving ? '保存中...' : '保存'}
				</AlertDialog.Action>
			</div>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
