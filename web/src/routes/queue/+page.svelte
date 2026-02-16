<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import {
		Card,
		CardContent,
		CardDescription,
		CardHeader,
		CardTitle
	} from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import {
		RefreshCw,
		ListTodo,
		Settings,
		Plus,
		Trash2,
		Clock,
		CheckCircle,
		AlertCircle,
		X
	} from '@lucide/svelte';
	import api from '$lib/api';
	import type { QueueStatusResponse } from '$lib/types';
	import { setBreadcrumb } from '$lib/stores/breadcrumb';
	import { runRequest } from '$lib/utils/request.js';
	import { formatTimestamp } from '$lib/utils/timezone';
	import { toast } from 'svelte-sonner';

	let queueStatus: QueueStatusResponse | null = null;
	let loading = true;
	let error: string | null = null;
	let refreshInterval: ReturnType<typeof setInterval> | null = null;
	let cancellingTaskIds: Record<string, boolean> = {};

	// 设置面包屑
	setBreadcrumb([
		{ label: '首页', href: '/' },
		{ label: '任务队列', href: '/queue' }
	]);

	// 获取队列状态
	async function fetchQueueStatus() {
		const response = await runRequest(() => api.getQueueStatus(), {
			context: '获取队列状态失败',
			onError: (err) => {
				error =
					err && typeof err === 'object' && 'message' in err
						? String(err.message)
						: '获取队列状态失败';
			}
		});
		if (!response) return;

		queueStatus = response.data;
		error = null;
	}

	// 手动刷新
	async function handleRefresh() {
		loading = true;
		await fetchQueueStatus();
		loading = false;
	}

	// 格式化时间
	function formatTime(timeString: string): string {
		const formatted = formatTimestamp(timeString, 'Asia/Shanghai', 'time');
		if (formatted === '无效时间' || formatted === '格式化失败') {
			return '无效时间';
		}
		return formatted;
	}

	// 获取任务类型的显示名称
	function getTaskTypeName(taskType: string): string {
		const typeMap: Record<string, string> = {
			delete_video_source: '删除视频源',
			delete_video: '删除视频',
			add_video_source: '添加视频源',
			update_config: '更新配置',
			reload_config: '重载配置'
		};
		return typeMap[taskType] || taskType;
	}

	// 获取任务类型的图标
	function getTaskTypeIcon(taskType: string) {
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		const iconMap: Record<string, any> = {
			delete_video_source: Trash2,
			delete_video: Trash2,
			add_video_source: Plus,
			update_config: Settings,
			reload_config: RefreshCw
		};
		return iconMap[taskType] || ListTodo;
	}

	function isCancelling(taskId: string): boolean {
		return !!cancellingTaskIds[taskId];
	}

	async function handleCancelTask(taskId: string) {
		if (!taskId || isCancelling(taskId)) return;

		cancellingTaskIds = { ...cancellingTaskIds, [taskId]: true };
		try {
			const response = await runRequest(() => api.cancelQueueTask(taskId), {
				context: '取消任务失败'
			});
			if (!response) return;

			toast.success('任务已取消', {
				description: response.data.message
			});
			await fetchQueueStatus();
		} finally {
			const { [taskId]: _removed, ...rest } = cancellingTaskIds;
			cancellingTaskIds = rest;
		}
	}

	// 获取队列状态颜色
	function getQueueStatusVariant(
		isProcessing: boolean,
		hasItems: boolean
	): 'default' | 'secondary' | 'destructive' | 'outline' {
		if (isProcessing) return 'destructive';
		if (hasItems) return 'secondary';
		return 'outline';
	}

	// 获取队列状态文本
	function getQueueStatusText(isProcessing: boolean, hasItems: boolean): string {
		if (isProcessing) return '处理中';
		if (hasItems) return '等待中';
		return '空闲';
	}

	onMount(() => {
		fetchQueueStatus().finally(() => {
			loading = false;
		});
		// 每5秒自动刷新一次
		refreshInterval = setInterval(fetchQueueStatus, 5000);
	});

	onDestroy(() => {
		if (refreshInterval) {
			clearInterval(refreshInterval);
		}
	});
</script>

<div class="container mx-auto px-4">
	<div class="mb-6 flex items-center justify-between">
		<div>
			<h1 class="text-3xl font-bold">任务队列</h1>
			<p class="text-muted-foreground mt-2">查看和管理系统任务队列状态</p>
		</div>
		<Button variant="outline" size="sm" onclick={handleRefresh} disabled={loading}>
			<RefreshCw class="mr-2 h-4 w-4 {loading ? 'animate-spin' : ''}" />
			刷新
		</Button>
	</div>

	{#if error}
		<Card class="border-destructive">
			<CardContent class="pt-6">
				<div class="text-destructive flex items-center gap-3">
					<AlertCircle class="h-5 w-5" />
					<span>{error}</span>
				</div>
			</CardContent>
		</Card>
	{:else if loading && !queueStatus}
		<div class="flex justify-center py-8">
			<div class="flex items-center gap-3">
				<RefreshCw class="h-5 w-5 animate-spin" />
				<span>加载队列状态中...</span>
			</div>
		</div>
	{:else if queueStatus}
		<!-- 系统状态总览 -->
		<div class="mb-6 grid gap-4 md:grid-cols-2 lg:grid-cols-4">
			<Card>
				<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
					<CardTitle class="text-sm font-medium">扫描状态</CardTitle>
					{#if queueStatus.is_scanning}
						<Clock class="h-4 w-4 text-orange-500" />
					{:else}
						<CheckCircle class="h-4 w-4 text-green-500" />
					{/if}
				</CardHeader>
				<CardContent>
					<div class="text-2xl font-bold">
						{queueStatus.is_scanning ? '扫描中' : '空闲'}
					</div>
					<p class="text-muted-foreground text-xs">
						{queueStatus.is_scanning ? '正在扫描视频源' : '等待下次扫描'}
					</p>
				</CardContent>
			</Card>

			<Card>
				<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
					<CardTitle class="text-sm font-medium">删除队列</CardTitle>
					<Trash2 class="text-muted-foreground h-4 w-4" />
				</CardHeader>
				<CardContent>
					<div class="text-2xl font-bold">{queueStatus.delete_queue.length}</div>
					<div class="flex items-center gap-2">
						<Badge
							variant={getQueueStatusVariant(
								queueStatus.delete_queue.is_processing,
								queueStatus.delete_queue.length > 0
							)}
						>
							{getQueueStatusText(
								queueStatus.delete_queue.is_processing,
								queueStatus.delete_queue.length > 0
							)}
						</Badge>
					</div>
				</CardContent>
			</Card>

			<Card>
				<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
					<CardTitle class="text-sm font-medium">添加队列</CardTitle>
					<Plus class="text-muted-foreground h-4 w-4" />
				</CardHeader>
				<CardContent>
					<div class="text-2xl font-bold">{queueStatus.add_queue.length}</div>
					<div class="flex items-center gap-2">
						<Badge
							variant={getQueueStatusVariant(
								queueStatus.add_queue.is_processing,
								queueStatus.add_queue.length > 0
							)}
						>
							{getQueueStatusText(
								queueStatus.add_queue.is_processing,
								queueStatus.add_queue.length > 0
							)}
						</Badge>
					</div>
				</CardContent>
			</Card>

			<Card>
				<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
					<CardTitle class="text-sm font-medium">配置队列</CardTitle>
					<Settings class="text-muted-foreground h-4 w-4" />
				</CardHeader>
				<CardContent>
					<div class="text-2xl font-bold">
						{queueStatus.config_queue.update_length + queueStatus.config_queue.reload_length}
					</div>
					<div class="flex items-center gap-2">
						<Badge
							variant={getQueueStatusVariant(
								queueStatus.config_queue.is_processing,
								queueStatus.config_queue.update_length + queueStatus.config_queue.reload_length > 0
							)}
						>
							{getQueueStatusText(
								queueStatus.config_queue.is_processing,
								queueStatus.config_queue.update_length + queueStatus.config_queue.reload_length > 0
							)}
						</Badge>
					</div>
				</CardContent>
			</Card>
		</div>

		<!-- 队列详情 -->
		<div class="grid gap-6 lg:grid-cols-3">
			<div class="space-y-6 lg:col-span-2">
				<!-- 删除队列 -->
				<Card>
					<CardHeader>
						<CardTitle class="flex items-center gap-2">
							<Trash2 class="h-5 w-5" />
							删除队列
							{#if queueStatus.delete_queue.is_processing}
								<Badge variant="destructive">处理中</Badge>
							{/if}
						</CardTitle>
						<CardDescription>等待处理的视频源删除任务</CardDescription>
					</CardHeader>
					<CardContent>
						{#if queueStatus.delete_queue.tasks.length === 0}
							<div class="text-muted-foreground flex items-center justify-center py-8">
								<div class="text-center">
									<CheckCircle class="mx-auto mb-3 h-12 w-12 opacity-50" />
									<p>队列为空</p>
								</div>
							</div>
						{:else}
							<div class="space-y-3">
								{#each queueStatus.delete_queue.tasks as task (task.task_id)}
									<div class="bg-muted/50 flex items-center justify-between rounded-lg p-3">
										<div class="flex items-center gap-3">
											<svelte:component
												this={getTaskTypeIcon(task.task_type)}
												class="text-muted-foreground h-4 w-4"
											/>
											<div>
												<p class="text-sm font-medium">{getTaskTypeName(task.task_type)}</p>
												<p class="text-muted-foreground text-xs">ID: {task.task_id}</p>
											</div>
										</div>
										<div class="flex items-center gap-2">
											<p class="text-muted-foreground text-xs">{formatTime(task.created_at)}</p>
											<Button
												variant="outline"
												size="sm"
												class="h-7 px-2 text-xs"
												data-glossary-term="取消任务"
												disabled={isCancelling(task.task_id)}
												onclick={() => handleCancelTask(task.task_id)}
											>
												<X class="mr-1 h-3.5 w-3.5" />
												{isCancelling(task.task_id) ? '取消中' : '取消'}
											</Button>
										</div>
									</div>
								{/each}
							</div>
						{/if}
					</CardContent>
				</Card>

				<!-- 添加队列 -->
				<Card>
					<CardHeader>
						<CardTitle class="flex items-center gap-2">
							<Plus class="h-5 w-5" />
							添加队列
							{#if queueStatus.add_queue.is_processing}
								<Badge variant="destructive">处理中</Badge>
							{/if}
						</CardTitle>
						<CardDescription>等待处理的视频源添加任务</CardDescription>
					</CardHeader>
					<CardContent>
						{#if queueStatus.add_queue.tasks.length === 0}
							<div class="text-muted-foreground flex items-center justify-center py-8">
								<div class="text-center">
									<CheckCircle class="mx-auto mb-3 h-12 w-12 opacity-50" />
									<p>队列为空</p>
								</div>
							</div>
						{:else}
							<div class="space-y-3">
								{#each queueStatus.add_queue.tasks as task (task.task_id)}
									<div class="bg-muted/50 flex items-center justify-between rounded-lg p-3">
										<div class="flex items-center gap-3">
											<svelte:component
												this={getTaskTypeIcon(task.task_type)}
												class="text-muted-foreground h-4 w-4"
											/>
											<div>
												<p class="text-sm font-medium">{getTaskTypeName(task.task_type)}</p>
												<p class="text-muted-foreground text-xs">ID: {task.task_id}</p>
											</div>
										</div>
										<div class="flex items-center gap-2">
											<p class="text-muted-foreground text-xs">{formatTime(task.created_at)}</p>
											<Button
												variant="outline"
												size="sm"
												class="h-7 px-2 text-xs"
												data-glossary-term="取消任务"
												disabled={isCancelling(task.task_id)}
												onclick={() => handleCancelTask(task.task_id)}
											>
												<X class="mr-1 h-3.5 w-3.5" />
												{isCancelling(task.task_id) ? '取消中' : '取消'}
											</Button>
										</div>
									</div>
								{/each}
							</div>
						{/if}
					</CardContent>
				</Card>

				<!-- 配置队列 -->
				<Card>
					<CardHeader>
						<CardTitle class="flex items-center gap-2">
							<Settings class="h-5 w-5" />
							配置队列
							{#if queueStatus.config_queue.is_processing}
								<Badge variant="destructive">处理中</Badge>
							{/if}
						</CardTitle>
						<CardDescription>等待处理的配置更新和重载任务</CardDescription>
					</CardHeader>
					<CardContent>
						{#if queueStatus.config_queue.update_tasks.length === 0 && queueStatus.config_queue.reload_tasks.length === 0}
							<div class="text-muted-foreground flex items-center justify-center py-8">
								<div class="text-center">
									<CheckCircle class="mx-auto mb-3 h-12 w-12 opacity-50" />
									<p>队列为空</p>
								</div>
							</div>
						{:else}
							<div class="grid gap-4 md:grid-cols-2">
								<!-- 更新配置任务 -->
								<div>
									<h4 class="mb-3 text-sm font-medium">
										更新配置任务 ({queueStatus.config_queue.update_length})
									</h4>
									{#if queueStatus.config_queue.update_tasks.length === 0}
										<p class="text-muted-foreground text-sm">暂无任务</p>
									{:else}
										<div class="space-y-3">
											{#each queueStatus.config_queue.update_tasks as task (task.task_id)}
												<div class="bg-muted/50 flex items-center justify-between rounded-lg p-3">
													<div class="flex items-center gap-3">
														<svelte:component
															this={getTaskTypeIcon(task.task_type)}
															class="text-muted-foreground h-4 w-4"
														/>
														<div>
															<p class="text-sm font-medium">{getTaskTypeName(task.task_type)}</p>
															<p class="text-muted-foreground text-xs">ID: {task.task_id}</p>
														</div>
													</div>
													<div class="flex items-center gap-2">
														<p class="text-muted-foreground text-xs">
															{formatTime(task.created_at)}
														</p>
														<Button
															variant="outline"
															size="sm"
															class="h-7 px-2 text-xs"
															data-glossary-term="取消任务"
															disabled={isCancelling(task.task_id)}
															onclick={() => handleCancelTask(task.task_id)}
														>
															<X class="mr-1 h-3.5 w-3.5" />
															{isCancelling(task.task_id) ? '取消中' : '取消'}
														</Button>
													</div>
												</div>
											{/each}
										</div>
									{/if}
								</div>

								<!-- 重载配置任务 -->
								<div>
									<h4 class="mb-3 text-sm font-medium">
										重载配置任务 ({queueStatus.config_queue.reload_length})
									</h4>
									{#if queueStatus.config_queue.reload_tasks.length === 0}
										<p class="text-muted-foreground text-sm">暂无任务</p>
									{:else}
										<div class="space-y-3">
											{#each queueStatus.config_queue.reload_tasks as task (task.task_id)}
												<div class="bg-muted/50 flex items-center justify-between rounded-lg p-3">
													<div class="flex items-center gap-3">
														<svelte:component
															this={getTaskTypeIcon(task.task_type)}
															class="text-muted-foreground h-4 w-4"
														/>
														<div>
															<p class="text-sm font-medium">{getTaskTypeName(task.task_type)}</p>
															<p class="text-muted-foreground text-xs">ID: {task.task_id}</p>
														</div>
													</div>
													<div class="flex items-center gap-2">
														<p class="text-muted-foreground text-xs">
															{formatTime(task.created_at)}
														</p>
														<Button
															variant="outline"
															size="sm"
															class="h-7 px-2 text-xs"
															data-glossary-term="取消任务"
															disabled={isCancelling(task.task_id)}
															onclick={() => handleCancelTask(task.task_id)}
														>
															<X class="mr-1 h-3.5 w-3.5" />
															{isCancelling(task.task_id) ? '取消中' : '取消'}
														</Button>
													</div>
												</div>
											{/each}
										</div>
									{/if}
								</div>
							</div>
						{/if}
					</CardContent>
				</Card>
			</div>

			<!-- 右侧说明面板 -->
			<div class="space-y-6">
				<!-- 任务队列说明 -->
				<Card class="sticky top-6">
					<CardHeader>
						<CardTitle class="text-lg">📋 任务队列说明</CardTitle>
						<CardDescription>了解任务队列的工作原理和状态含义</CardDescription>
					</CardHeader>
					<CardContent class="space-y-6">
						<!-- 队列类型说明 -->
						<div>
							<h4 class="mb-3 flex items-center gap-2 font-medium">
								<ListTodo class="h-4 w-4" />
								队列类型
							</h4>
							<div class="space-y-3 text-sm">
								<div class="flex items-start gap-3 rounded-lg bg-red-50 p-3 dark:bg-red-950/20">
									<Trash2 class="mt-0.5 h-4 w-4 text-red-600" />
									<div>
										<p class="font-medium text-red-900 dark:text-red-100">删除队列</p>
										<p class="text-xs text-red-700 dark:text-red-300">
											处理视频源删除操作，确保数据一致性
										</p>
									</div>
								</div>
								<div class="flex items-start gap-3 rounded-lg bg-green-50 p-3 dark:bg-green-950/20">
									<Plus class="mt-0.5 h-4 w-4 text-green-600" />
									<div>
										<p class="font-medium text-green-900 dark:text-green-100">添加队列</p>
										<p class="text-xs text-green-700 dark:text-green-300">
											处理新视频源的添加和初始化
										</p>
									</div>
								</div>
								<div class="flex items-start gap-3 rounded-lg bg-blue-50 p-3 dark:bg-blue-950/20">
									<Settings class="mt-0.5 h-4 w-4 text-blue-600" />
									<div>
										<p class="font-medium text-blue-900 dark:text-blue-100">配置队列</p>
										<p class="text-xs text-blue-700 dark:text-blue-300">处理系统配置的更新和重载</p>
									</div>
								</div>
							</div>
						</div>

						<!-- 状态说明 -->
						<div>
							<h4 class="mb-3 flex items-center gap-2 font-medium">
								<Clock class="h-4 w-4" />
								状态说明
							</h4>
							<div class="space-y-2 text-sm">
								<div class="flex items-center gap-3">
									<Badge variant="outline" class="w-16 justify-center">空闲</Badge>
									<span class="text-muted-foreground">队列为空，没有待处理任务</span>
								</div>
								<div class="flex items-center gap-3">
									<Badge variant="secondary" class="w-16 justify-center">等待中</Badge>
									<span class="text-muted-foreground">有任务排队，等待处理</span>
								</div>
								<div class="flex items-center gap-3">
									<Badge variant="destructive" class="w-16 justify-center">处理中</Badge>
									<span class="text-muted-foreground">正在执行队列中的任务</span>
								</div>
							</div>
						</div>

						<!-- 处理机制 -->
						<div>
							<h4 class="mb-3 flex items-center gap-2 font-medium">
								<RefreshCw class="h-4 w-4" />
								处理机制
							</h4>
							<div class="text-muted-foreground space-y-2 text-sm">
								<div class="flex items-start gap-2">
									<span class="font-mono text-blue-600">1.</span>
									<span>扫描期间的所有操作会自动进入对应队列</span>
								</div>
								<div class="flex items-start gap-2">
									<span class="font-mono text-blue-600">2.</span>
									<span>扫描完成后按优先级处理：配置 → 删除 → 添加</span>
								</div>
								<div class="flex items-start gap-2">
									<span class="font-mono text-blue-600">3.</span>
									<span>同类型任务按时间顺序依次执行</span>
								</div>
								<div class="flex items-start gap-2">
									<span class="font-mono text-blue-600">4.</span>
									<span>页面每5秒自动刷新状态</span>
								</div>
							</div>
						</div>

						<!-- 注意事项 -->
						<div>
							<h4 class="mb-3 flex items-center gap-2 font-medium">
								<AlertCircle class="h-4 w-4" />
								注意事项
							</h4>
							<div
								class="text-muted-foreground space-y-2 rounded-lg bg-amber-50 p-3 text-sm dark:bg-amber-950/20"
							>
								<p>• 扫描进行时，手动操作会进入队列等待</p>
								<p>• 队列处理期间请避免频繁操作</p>
								<p>• 配置更改会影响所有后续任务执行</p>
								<p>• 删除操作不可逆，请谨慎操作</p>
							</div>
						</div>
					</CardContent>
				</Card>
			</div>
		</div>

		<!-- 简化版说明信息 -->
		<Card class="mt-6">
			<CardHeader>
				<CardTitle class="text-lg">快速参考</CardTitle>
			</CardHeader>
			<CardContent class="space-y-4">
				<div class="grid gap-4 md:grid-cols-2">
					<div>
						<h4 class="mb-2 font-medium">常见任务类型</h4>
						<ul class="text-muted-foreground space-y-1 text-sm">
							<li>• <strong>delete_video_source:</strong> 删除视频源</li>
							<li>• <strong>add_video_source:</strong> 添加视频源</li>
							<li>• <strong>update_config:</strong> 更新系统配置</li>
							<li>• <strong>reload_config:</strong> 重载配置文件</li>
						</ul>
					</div>
					<div>
						<h4 class="mb-2 font-medium">性能提示</h4>
						<ul class="text-muted-foreground space-y-1 text-sm">
							<li>• 批量操作会自动合并到队列中</li>
							<li>• 避免在扫描期间进行大量操作</li>
							<li>• 队列处理期间系统性能可能下降</li>
							<li>• 建议在空闲时间进行配置更改</li>
						</ul>
					</div>
				</div>
			</CardContent>
		</Card>
	{/if}
</div>
