<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { createEventDispatcher } from 'svelte';
	import { api } from '$lib/api';
	import { toast } from 'svelte-sonner';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Label } from '$lib/components/ui/label';
	import { Button } from '$lib/components/ui/button';

	export let isOpen = false;
	export let sourceName = '';
	export let sourceType = '';
	export let sourceId = 0;
export let initialVideoPrompt = '';
export let initialAudioPrompt = '';
// 高级选项初始值（从视频源加载）
export let initialEnableMultiPage = false;
export let initialEnableCollection = false;
export let initialEnableBangumi = false;
export let initialRenameParentDir = false;

	const dispatch = createEventDispatcher<{
		complete: { renamed: number; skipped: number; failed: number };
		cancel: void;
	}>();

	let videoPrompt = '';
	let audioPrompt = '';

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
	enableMultiPage = initialEnableMultiPage;
	enableCollection = initialEnableCollection;
	enableBangumi = initialEnableBangumi;
	renameParentDir = initialRenameParentDir;
	// 如果有任何高级选项被启用，自动展开高级选项面板
	showAdvancedOptions =
		initialEnableMultiPage || initialEnableCollection || initialEnableBangumi || initialRenameParentDir;
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

	// 执行批量重命名（异步，不阻塞界面）
	async function handleExecute() {
		// 立即关闭对话框
		isOpen = false;

		// 显示开始处理的提示
		toast.info('AI批量重命名已开始', {
			description: `正在处理 ${getSourceTypeLabel(sourceType)}「${sourceName}」，请稍候...`
		});

		// 在后台执行重命名
		executeRenameInBackground();
	}

	// 后台执行重命名逻辑
	async function executeRenameInBackground() {
		try {
			// 执行批量重命名，传递自定义提示词和高级选项
			const result = await api.aiRenameHistory(
				sourceType,
				sourceId,
				videoPrompt.trim(),
			audioPrompt.trim(),
			enableMultiPage,
			enableCollection,
			enableBangumi,
			renameParentDir
		);

			if (result.data.success) {
				// 重命名成功后，同步更新该源的 AI 重命名设置（包括提示词、开关和高级选项）
				try {
					await api.updateVideoSourceDownloadOptions(sourceType, sourceId, {
						ai_rename: true,
						ai_rename_video_prompt: videoPrompt.trim(),
						ai_rename_audio_prompt: audioPrompt.trim(),
						ai_rename_enable_multi_page: enableMultiPage,
						ai_rename_enable_collection: enableCollection,
						ai_rename_enable_bangumi: enableBangumi,
						ai_rename_rename_parent_dir: renameParentDir
					});
				} catch (updateError) {
					console.warn('更新AI重命名设置失败:', updateError);
				}

				toast.success('AI批量重命名完成', {
					description: `重命名 ${result.data.renamed_count} 个，跳过 ${result.data.skipped_count} 个，失败 ${result.data.failed_count} 个。已自动开启AI重命名功能并同步设置。`
				});
				dispatch('complete', {
					renamed: result.data.renamed_count,
					skipped: result.data.skipped_count,
					failed: result.data.failed_count
				});
			} else {
				toast.error('批量重命名失败', { description: result.data.message });
			}
		} catch (error) {
			console.error('批量重命名失败:', error);
			toast.error('批量重命名失败', { description: (error as Error).message });
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
			<AlertDialog.Title
				title={sourceName ? `AI批量重命名历史文件 - ${sourceName}` : 'AI批量重命名历史文件'}
			>
				AI批量重命名历史文件
			</AlertDialog.Title>
			<AlertDialog.Description>
				为 {getSourceTypeLabel(sourceType)} 「{sourceName}」 下的所有已下载文件执行AI重命名
			</AlertDialog.Description>
		</AlertDialog.Header>

		<div class="space-y-4 py-4">
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
								id="enable-multi-page"
								bind:checked={enableMultiPage}
								class="border-input h-4 w-4 rounded border"
							/>
							<Label for="enable-multi-page" class="text-sm leading-none font-medium">
								对多P视频启用AI重命名
							</Label>
						</div>
						<div class="flex items-center space-x-2">
							<input
								type="checkbox"
								id="enable-collection"
								bind:checked={enableCollection}
								class="border-input h-4 w-4 rounded border"
							/>
							<Label for="enable-collection" class="text-sm leading-none font-medium">
								对合集视频启用AI重命名
							</Label>
						</div>
						<div class="flex items-center space-x-2">
							<input
								type="checkbox"
								id="enable-bangumi"
								bind:checked={enableBangumi}
								class="border-input h-4 w-4 rounded border"
							/>
							<Label for="enable-bangumi" class="text-sm leading-none font-medium">
								对番剧启用AI重命名
							</Label>
						</div>
						<div class="flex items-center space-x-2">
							<input
								type="checkbox"
								id="rename-parent-dir"
								bind:checked={renameParentDir}
								class="border-input h-4 w-4 rounded border"
							/>
							<Label for="rename-parent-dir" class="text-sm leading-none font-medium">
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

			<!-- 提示信息 -->
			<div
				class="rounded-lg border border-blue-200 bg-blue-50 p-3 dark:border-blue-800 dark:bg-blue-950"
			>
				<p class="text-xs text-blue-700 dark:text-blue-300">
					点击"开始重命名"后，对话框将关闭，任务会在后台执行。完成后将显示通知，并自动为该视频源开启AI重命名功能。
				</p>
			</div>

			<!-- 警告提示 -->
			<div
				class="rounded-lg border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950"
			>
				<p class="text-xs text-amber-700 dark:text-amber-300">
					注意：此操作会重命名该视频源下所有已下载的文件，包括视频、音频及其附属文件（NFO、字幕、封面等）。处理时间取决于文件数量和AI响应速度。
				</p>
			</div>
		</div>

		<AlertDialog.Footer>
			<AlertDialog.Cancel onclick={handleCancel}>取消</AlertDialog.Cancel>
			<Button onclick={handleExecute}>开始重命名</Button>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
