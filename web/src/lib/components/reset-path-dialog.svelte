<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { createEventDispatcher } from 'svelte';
	import type { ResetVideoSourcePathRequest } from '$lib/types';

	export let isOpen = false;
	export let sourceName = '';
	export let sourceType = '';
	export let currentPath = '';

	const dispatch = createEventDispatcher<{
		confirm: ResetVideoSourcePathRequest;
		cancel: void;
	}>();

	let newPath = '';
	let applyRenameRules = true;
	let cleanEmptyFolders = true;
	let isProcessing = false;

	// 重置状态
	function resetState() {
		newPath = currentPath || '';
		applyRenameRules = true;
		cleanEmptyFolders = true;
		isProcessing = false;
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

	// 处理确认重设
	async function handleConfirm() {
		if (!newPath.trim()) {
			return;
		}

		isProcessing = true;
		try {
			dispatch('confirm', {
				new_path: newPath.trim(),
				apply_rename_rules: applyRenameRules,
				clean_empty_folders: cleanEmptyFolders
			});
			isOpen = false;
		} catch (error) {
			console.error('路径重设失败:', error);
		} finally {
			isProcessing = false;
		}
	}

	// 处理取消
	function handleCancel() {
		if (isProcessing) return;
		dispatch('cancel');
		isOpen = false;
	}

	// 检查是否可以确认
	$: canConfirm = newPath.trim() && newPath.trim() !== currentPath && !isProcessing;
</script>

<AlertDialog.Root bind:open={isOpen}>
	<AlertDialog.Content class="max-w-lg">
		<AlertDialog.Header>
			<AlertDialog.Title
				class="flex items-center gap-2 text-blue-600 dark:text-blue-400"
				title={sourceName ? `重设视频源路径 - ${sourceName}` : '重设视频源路径'}
			>
				<svg class="h-5 w-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						stroke-width="2"
						d="M4 7v10c0 2.21 3.582 4 8 4s8-1.79 8-4V7M4 7c0 2.21 3.582 4 8 4s8-1.79 8-4M4 7c0-2.21 3.582-4 8-4s8 1.79 8 4"
					/>
				</svg>
				重设视频源路径
			</AlertDialog.Title>
			<AlertDialog.Description class="space-y-4">
				<div
					class="rounded-lg border border-blue-200 bg-blue-50 p-3 dark:border-blue-800 dark:bg-blue-950"
				>
					<p class="text-sm font-medium text-blue-800 dark:text-blue-200">📁 路径重设说明</p>
					<p class="mt-1 text-xs text-blue-700 dark:text-blue-300">
						此操作将更改视频源的存储路径，并可选择移动现有文件到新位置。
					</p>
				</div>

				<div class="space-y-2">
					<div class="flex items-center gap-2 text-sm">
						<span class="font-medium">类型：</span>
						<span
							class="rounded bg-blue-100 px-2 py-1 text-xs text-blue-800 dark:bg-blue-900 dark:text-blue-200"
						>
							{getSourceTypeLabel(sourceType)}
						</span>
					</div>
					<div class="flex items-center gap-2 text-sm">
						<span class="font-medium">名称：</span>
						<span class="font-mono text-gray-800 dark:text-gray-200">"{sourceName}"</span>
					</div>
					<div class="flex items-start gap-2 text-sm">
						<span class="font-medium">当前路径：</span>
						<span class="font-mono break-all text-gray-600 dark:text-gray-400">{currentPath}</span>
					</div>
				</div>

				<div class="space-y-3">
					<div class="space-y-2">
						<label for="new-path" class="text-sm font-medium text-gray-700 dark:text-gray-300">
							新路径 <span class="text-red-500">*</span>
						</label>
						<input
							id="new-path"
							type="text"
							bind:value={newPath}
							placeholder="输入新的存储路径，例如：/downloads/videos"
							class="w-full rounded-md border border-gray-300 px-3 py-2 text-sm focus:border-blue-500 focus:ring-2 focus:ring-blue-500 focus:outline-none dark:border-gray-600 dark:bg-gray-700 dark:text-gray-200 dark:placeholder-gray-400 dark:focus:border-blue-400 dark:focus:ring-blue-400"
							disabled={isProcessing}
						/>
					</div>

					<div class="space-y-3">
						<div class="flex items-start gap-3 rounded-lg border p-3 dark:border-gray-600">
							<input
								type="checkbox"
								id="apply-rename-rules"
								bind:checked={applyRenameRules}
								class="mt-1 h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
								disabled={isProcessing}
							/>
							<div class="flex-1 space-y-1">
								<label
									for="apply-rename-rules"
									class="flex cursor-pointer items-center gap-2 text-sm font-medium"
								>
									<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
										<path
											stroke-linecap="round"
											stroke-linejoin="round"
											stroke-width="2"
											d="M8 7h12m0 0l-4-4m4 4l-4 4m0 6H4m0 0l4 4m-4-4l4-4"
										/>
									</svg>
									应用四步重命名原则移动文件
								</label>
								<p class="text-xs text-gray-600 dark:text-gray-400">
									勾选后将使用安全的四步重命名原则移动现有文件到新路径。
									这可以避免数据丢失和文件名冲突。
								</p>
							</div>
						</div>

						{#if applyRenameRules}
							<div class="flex items-start gap-3 rounded-lg border p-3 dark:border-gray-600">
								<input
									type="checkbox"
									id="clean-empty-folders"
									bind:checked={cleanEmptyFolders}
									class="mt-1 h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
									disabled={isProcessing}
								/>
								<div class="flex-1 space-y-1">
									<label
										for="clean-empty-folders"
										class="flex cursor-pointer items-center gap-2 text-sm font-medium"
									>
										<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
											<path
												stroke-linecap="round"
												stroke-linejoin="round"
												stroke-width="2"
												d="M9 13h6m2 8H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
											/>
										</svg>
										清理空的原始文件夹
									</label>
									<p class="text-xs text-gray-600 dark:text-gray-400">
										移动文件后，删除原始路径中的空文件夹。
									</p>
								</div>
							</div>
						{/if}
					</div>

					{#if applyRenameRules}
						<div
							class="rounded-lg border border-green-200 bg-green-50 p-3 dark:border-green-800 dark:bg-green-950"
						>
							<div class="flex items-start gap-2">
								<svg
									class="mt-0.5 h-4 w-4 flex-shrink-0 text-green-600"
									fill="none"
									stroke="currentColor"
									viewBox="0 0 24 24"
								>
									<path
										stroke-linecap="round"
										stroke-linejoin="round"
										stroke-width="2"
										d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
									/>
								</svg>
								<div class="text-xs text-green-800 dark:text-green-200">
									<p class="font-medium">四步重命名原则说明：</p>
									<ol class="mt-1 list-inside list-decimal space-y-0.5 text-xs">
										<li>重命名为临时名称（避免冲突）</li>
										<li>移动到目标目录（使用临时名称）</li>
										<li>重命名为最终名称</li>
										<li>清理临时文件（如需要）</li>
									</ol>
									<p class="mt-1 text-xs">此方法可最大程度避免文件丢失和名称冲突。</p>
								</div>
							</div>
						</div>
					{:else}
						<div
							class="rounded-lg border border-yellow-200 bg-yellow-50 p-3 dark:border-yellow-800 dark:bg-yellow-950"
						>
							<div class="flex items-start gap-2">
								<svg
									class="mt-0.5 h-4 w-4 flex-shrink-0 text-yellow-600"
									fill="none"
									stroke="currentColor"
									viewBox="0 0 24 24"
								>
									<path
										stroke-linecap="round"
										stroke-linejoin="round"
										stroke-width="2"
										d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-2.5L13.732 4c-.77-.833-1.732-.833-2.5 0L4.268 19.5c-.77.833.192 2.5 1.732 2.5z"
									/>
								</svg>
								<div class="text-xs text-yellow-800 dark:text-yellow-200">
									<p class="font-medium">注意：</p>
									<p class="mt-1">
										未勾选文件移动选项时，只会更新数据库中的路径配置，
										不会移动现有文件。您需要手动移动文件到新路径。
									</p>
								</div>
							</div>
						</div>
					{/if}
				</div>
			</AlertDialog.Description>
		</AlertDialog.Header>
		<AlertDialog.Footer class="flex justify-end gap-3 pt-4">
			<button
				type="button"
				class="rounded-md border border-gray-300 bg-white px-4 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 focus:ring-2 focus:ring-indigo-500 focus:ring-offset-2 focus:outline-none disabled:cursor-not-allowed disabled:opacity-50 dark:border-gray-600 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600 dark:focus:ring-offset-gray-800"
				disabled={isProcessing}
				on:click={handleCancel}
			>
				取消
			</button>
			<button
				type="button"
				class="rounded-md border border-transparent bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
				disabled={!canConfirm || isProcessing}
				on:click={handleConfirm}
			>
				{#if isProcessing}
					<svg class="mr-2 inline h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24">
						<circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"
						></circle>
						<path
							class="opacity-75"
							fill="currentColor"
							d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
						></path>
					</svg>
					处理中...
				{:else}
					确认重设
				{/if}
			</button>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
