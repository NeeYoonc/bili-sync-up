<!-- 可复用的状态任务卡片组件 -->
<script lang="ts">
	import { Button } from '$lib/components/ui/button/index.js';

	export let taskName: string;
	export let currentStatus: number;
	export let originalStatus: number;
	export let onStatusChange: (newStatus: number) => void;
	export let onReset: () => void;
	export let disabled: boolean = false;

	// 检查是否为关键的下载任务
	$: isDownloadTask = taskName === '分P下载';

	// 获取状态显示信息
	function getStatusInfo(value: number) {
		if (value === 7) return { label: '已完成', class: 'text-green-600', dotClass: 'bg-green-500' };
		if (value >= 1 && value <= 4)
			return { label: `失败${value}次`, class: 'text-red-600', dotClass: 'bg-red-500' };
		return { label: '未开始', class: 'text-yellow-600', dotClass: 'bg-yellow-500' };
	}

	$: statusInfo = getStatusInfo(currentStatus);
	$: isModified = currentStatus !== originalStatus;
</script>

<div
	class="status-task-card bg-background hover:bg-muted/30 flex flex-col gap-2 rounded-md border p-2.5 transition-colors sm:flex-row sm:items-center sm:justify-between sm:gap-3 sm:p-3 {isModified
		? 'border-blue-200 ring-2 ring-blue-500/20'
		: isDownloadTask
			? 'border-orange-200 bg-orange-50/30'
			: ''}"
>
	<div class="flex items-center gap-3">
		<div>
			<div class="flex items-center gap-2">
				<span
					class="status-task-title text-sm font-medium leading-5 {isDownloadTask ? 'text-orange-700' : ''}"
					title={taskName}
				>{taskName}</span
				>
				{#if isDownloadTask}
					<span class="rounded-full bg-orange-100 px-2 py-0.5 text-xs font-medium text-orange-700"
						>关键</span
					>
				{/if}
				{#if isModified}
					<span class="hidden text-xs font-medium text-blue-600 sm:inline">已修改</span>
					<div class="h-2 w-2 rounded-full bg-blue-500 sm:hidden" title="已修改"></div>
				{/if}
			</div>
			<div class="mt-0.5 flex items-center gap-1.5">
				<div class="h-1.5 w-1.5 rounded-full {statusInfo.dotClass}"></div>
				<span class="text-xs {statusInfo.class}">{statusInfo.label}</span>
			</div>
		</div>
	</div>
	<div class="status-task-actions grid w-full grid-cols-2 gap-1.5 sm:flex sm:w-auto">
		{#if isModified}
			<Button
				variant="ghost"
				size="sm"
				onclick={onReset}
				{disabled}
				class="col-span-2 h-7.5 w-full cursor-pointer px-3 text-xs text-gray-600 hover:bg-gray-100 sm:col-auto sm:h-8 sm:min-w-[60px] sm:w-auto"
				title="恢复到原始状态"
			>
				重置
			</Button>
		{/if}
		<Button
			variant={currentStatus === 0 ? 'default' : 'outline'}
			size="sm"
			onclick={() => onStatusChange(0)}
			{disabled}
			class="h-7.5 w-full cursor-pointer px-3 text-xs sm:h-8 sm:min-w-[60px] sm:w-auto {currentStatus === 0
				? 'border-yellow-600 bg-yellow-600 font-medium text-white hover:bg-yellow-700'
				: 'hover:border-yellow-400 hover:bg-yellow-50 hover:text-yellow-700'}"
			title={`将“${taskName}”标记为未开始`}
		>
			未开始
		</Button>
		<Button
			variant={currentStatus === 7 ? 'default' : 'outline'}
			size="sm"
			onclick={() => onStatusChange(7)}
			{disabled}
			class="h-7.5 w-full cursor-pointer px-3 text-xs sm:h-8 sm:min-w-[60px] sm:w-auto {currentStatus === 7
				? 'border-green-600 bg-green-600 font-medium text-white hover:bg-green-700'
				: 'hover:border-green-400 hover:bg-green-50 hover:text-green-700'}"
			title={`将“${taskName}”标记为已完成`}
		>
			已完成
		</Button>
	</div>
</div>

<style>
	@media (max-height: 700px) {
		.status-task-card {
			padding: 0.625rem 0.75rem;
			gap: 0.5rem;
		}

		.status-task-title {
			font-size: 0.9375rem;
			line-height: 1.25rem;
		}

		.status-task-actions :global(button) {
			height: 2rem;
		}
	}
</style>
