<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import { createEventDispatcher } from 'svelte';
	import { api } from '$lib/api';

	export let isOpen = false;
	export let sourceName = '';
	export let sourceType = '';
	export let sourceId = 0;
	export let initialBlacklistKeywords: string[] = [];
	export let initialWhitelistKeywords: string[] = [];
	export let initialCaseSensitive: boolean = true;
	export let initialMinDurationSeconds: number | null = null;
	export let initialMaxDurationSeconds: number | null = null;
	export let initialPublishedAfter = '';
	export let initialPublishedBefore = '';

	const dispatch = createEventDispatcher<{
		save: {
			blacklistKeywords: string[];
			whitelistKeywords: string[];
			caseSensitive: boolean;
			minDurationSeconds: number | null;
			maxDurationSeconds: number | null;
			publishedAfter: string;
			publishedBefore: string;
		};
		cancel: void;
	}>();

	let blacklistKeywords: string[] = [];
	let whitelistKeywords: string[] = [];
	let caseSensitive: boolean = true;
	let minDurationSeconds: string | number = '';
	let maxDurationSeconds: string | number = '';
	let publishedAfter = '';
	let publishedBefore = '';
	let newBlacklistKeyword = '';
	let newWhitelistKeyword = '';
	let isLoading = false;
	let isSaving = false;
	let blacklistValidationError = '';
	let whitelistValidationError = '';
	let advancedValidationError = '';

	// 当前选中的标签页：'whitelist' 或 'blacklist'
	let activeTab: 'whitelist' | 'blacklist' = 'whitelist';

	// 重置状态
	function resetState() {
		blacklistKeywords = [...initialBlacklistKeywords];
		whitelistKeywords = [...initialWhitelistKeywords];
		caseSensitive = initialCaseSensitive;
		minDurationSeconds =
			initialMinDurationSeconds !== null ? String(initialMinDurationSeconds) : '';
		maxDurationSeconds =
			initialMaxDurationSeconds !== null ? String(initialMaxDurationSeconds) : '';
		publishedAfter = initialPublishedAfter;
		publishedBefore = initialPublishedBefore;
		newBlacklistKeyword = '';
		newWhitelistKeyword = '';
		blacklistValidationError = '';
		whitelistValidationError = '';
		advancedValidationError = '';
		isLoading = false;
		isSaving = false;
		activeTab = 'whitelist';
	}

	// 当对话框打开时重置状态
	$: if (isOpen) {
		resetState();
		loadKeywords();
	}

	// 加载关键词
	async function loadKeywords() {
		if (!sourceId) return;

		isLoading = true;
		try {
			const response = await api.getVideoSourceKeywordFilters(sourceType, sourceId);
			if (response.status_code === 200) {
				blacklistKeywords = response.data.blacklist_keywords || [];
				whitelistKeywords = response.data.whitelist_keywords || [];
				caseSensitive = response.data.case_sensitive ?? true;
				minDurationSeconds =
					response.data.min_duration_seconds !== undefined
						? String(response.data.min_duration_seconds)
						: '';
				maxDurationSeconds =
					response.data.max_duration_seconds !== undefined
						? String(response.data.max_duration_seconds)
						: '';
				publishedAfter = response.data.published_after || '';
				publishedBefore = response.data.published_before || '';
			}
		} catch (error) {
			console.error('加载关键词失败:', error);
		} finally {
			isLoading = false;
		}
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

	// 验证正则表达式
	async function validatePattern(pattern: string): Promise<{ valid: boolean; error?: string }> {
		try {
			const response = await api.validateRegex(pattern);
			if (response.status_code === 200) {
				return {
					valid: response.data.valid,
					error: response.data.error
				};
			}
			return { valid: false, error: '验证请求失败' };
		} catch {
			return { valid: false, error: '网络错误' };
		}
	}

	// 检查关键词是否在另一个列表中存在（互斥校验）
	function checkMutualExclusivity(
		keyword: string,
		targetList: 'blacklist' | 'whitelist'
	): string | null {
		if (targetList === 'blacklist' && whitelistKeywords.includes(keyword)) {
			return '该关键词已存在于白名单中，同一关键词不能同时出现在黑名单和白名单';
		}
		if (targetList === 'whitelist' && blacklistKeywords.includes(keyword)) {
			return '该关键词已存在于黑名单中，同一关键词不能同时出现在黑名单和白名单';
		}
		return null;
	}

	function normalizeDurationInput(value: string | number | null | undefined): string {
		if (value === null || value === undefined) {
			return '';
		}
		return String(value).trim();
	}

	function parseDurationInput(
		value: string | number | null | undefined,
		fieldName: string
	): number | null {
		const trimmed = normalizeDurationInput(value);
		if (!trimmed) {
			return null;
		}

		if (!/^\d+$/.test(trimmed)) {
			throw new Error(`${fieldName}必须为非负整数秒数`);
		}

		return Number(trimmed);
	}

	// 添加黑名单关键词
	async function addBlacklistKeyword() {
		const pattern = newBlacklistKeyword.trim();
		if (!pattern) {
			blacklistValidationError = '请输入关键词';
			return;
		}

		if (blacklistKeywords.includes(pattern)) {
			blacklistValidationError = '该关键词已存在于黑名单中';
			return;
		}

		// 互斥校验
		const mutualError = checkMutualExclusivity(pattern, 'blacklist');
		if (mutualError) {
			blacklistValidationError = mutualError;
			return;
		}

		// 验证正则表达式
		const result = await validatePattern(pattern);
		if (!result.valid) {
			blacklistValidationError = result.error || '无效的正则表达式';
			return;
		}

		blacklistKeywords = [...blacklistKeywords, pattern];
		newBlacklistKeyword = '';
		blacklistValidationError = '';
	}

	// 添加白名单关键词
	async function addWhitelistKeyword() {
		const pattern = newWhitelistKeyword.trim();
		if (!pattern) {
			whitelistValidationError = '请输入关键词';
			return;
		}

		if (whitelistKeywords.includes(pattern)) {
			whitelistValidationError = '该关键词已存在于白名单中';
			return;
		}

		// 互斥校验
		const mutualError = checkMutualExclusivity(pattern, 'whitelist');
		if (mutualError) {
			whitelistValidationError = mutualError;
			return;
		}

		// 验证正则表达式
		const result = await validatePattern(pattern);
		if (!result.valid) {
			whitelistValidationError = result.error || '无效的正则表达式';
			return;
		}

		whitelistKeywords = [...whitelistKeywords, pattern];
		newWhitelistKeyword = '';
		whitelistValidationError = '';
	}

	// 删除黑名单关键词
	function removeBlacklistKeyword(index: number) {
		blacklistKeywords = blacklistKeywords.filter((_, i) => i !== index);
	}

	// 删除白名单关键词
	function removeWhitelistKeyword(index: number) {
		whitelistKeywords = whitelistKeywords.filter((_, i) => i !== index);
	}

	// 保存关键词
	async function handleSave() {
		advancedValidationError = '';

		let minDuration: number | null = null;
		let maxDuration: number | null = null;
		try {
			minDuration = parseDurationInput(minDurationSeconds, '最短时长');
			maxDuration = parseDurationInput(maxDurationSeconds, '最长时长');
		} catch (error) {
			advancedValidationError = (error as Error).message;
			return;
		}

		if (minDuration !== null && maxDuration !== null && minDuration > maxDuration) {
			advancedValidationError = '最短时长不能大于最长时长';
			return;
		}

		if (publishedAfter && publishedBefore && publishedAfter > publishedBefore) {
			advancedValidationError = '投稿起始日期不能晚于投稿截止日期';
			return;
		}

		isSaving = true;
		try {
			const response = await api.updateVideoSourceKeywordFilters(
				sourceType,
				sourceId,
				blacklistKeywords,
				whitelistKeywords,
				caseSensitive,
				minDuration,
				maxDuration,
				publishedAfter,
				publishedBefore
			);
			if (response.status_code === 200) {
				dispatch('save', {
					blacklistKeywords,
					whitelistKeywords,
					caseSensitive,
					minDurationSeconds: minDuration,
					maxDurationSeconds: maxDuration,
					publishedAfter,
					publishedBefore
				});
				isOpen = false;
			} else {
				blacklistValidationError = '保存失败';
			}
		} catch (error) {
			console.error('保存关键词失败:', error);
			blacklistValidationError = '保存时发生错误';
		} finally {
			isSaving = false;
		}
	}

	// 处理取消
	function handleCancel() {
		if (isSaving) return;
		dispatch('cancel');
		isOpen = false;
	}

	// 处理键盘事件 - 黑名单
	function handleBlacklistKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter') {
			event.preventDefault();
			addBlacklistKeyword();
		}
	}

	// 处理键盘事件 - 白名单
	function handleWhitelistKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter') {
			event.preventDefault();
			addWhitelistKeyword();
		}
	}
</script>

<AlertDialog.Root bind:open={isOpen}>
	<AlertDialog.Content class="max-h-[90vh] max-w-2xl overflow-y-auto">
		<AlertDialog.Header>
			<AlertDialog.Title
				class="flex items-center gap-2 text-purple-600 dark:text-purple-400"
				title={sourceName ? `关键词过滤器 - ${sourceName}` : '关键词过滤器'}
			>
				<svg class="h-5 w-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						stroke-width="2"
						d="M3 4a1 1 0 011-1h16a1 1 0 011 1v2.586a1 1 0 01-.293.707l-6.414 6.414a1 1 0 00-.293.707V17l-4 4v-6.586a1 1 0 00-.293-.707L3.293 7.293A1 1 0 013 6.586V4z"
					/>
				</svg>
				关键词过滤器
			</AlertDialog.Title>
			<AlertDialog.Description class="space-y-4">
				<!-- 视频源信息 -->
				<div class="space-y-2">
					<div class="flex items-center gap-2 text-sm">
						<span class="font-medium">类型：</span>
						<span
							class="rounded bg-purple-100 px-2 py-1 text-xs text-purple-800 dark:bg-purple-900 dark:text-purple-200"
						>
							{getSourceTypeLabel(sourceType)}
						</span>
					</div>
					<div class="flex items-center gap-2 text-sm">
						<span class="font-medium">名称：</span>
						<span class="font-mono text-gray-800 dark:text-gray-200">"{sourceName}"</span>
					</div>
				</div>

				<!-- 过滤逻辑说明 -->
				<div
					class="rounded-lg border border-blue-200 bg-blue-50 p-3 dark:border-blue-800 dark:bg-blue-950"
				>
					<p class="text-sm font-medium text-blue-800 dark:text-blue-200">过滤逻辑说明</p>
					<ul class="mt-1 space-y-1 text-xs text-blue-700 dark:text-blue-300">
						<li>1. 如果设置了白名单，视频必须匹配至少一个白名单关键词才会被下载</li>
						<li>2. 匹配黑名单的视频会被排除（即使通过了白名单）</li>
						<li>3. 同一关键词不能同时出现在黑名单和白名单中</li>
					</ul>
				</div>

				<!-- 大小写敏感设置 -->
				<div
					class="flex items-center justify-between rounded-lg border border-gray-200 bg-gray-50 px-4 py-3 dark:border-gray-700 dark:bg-gray-800"
				>
					<div class="flex items-center gap-2">
						<svg
							class="h-4 w-4 text-gray-600 dark:text-gray-400"
							fill="none"
							stroke="currentColor"
							viewBox="0 0 24 24"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								stroke-width="2"
								d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
							/>
						</svg>
						<span class="text-sm font-medium text-gray-700 dark:text-gray-300">区分大小写</span>
					</div>
					<label class="relative inline-flex cursor-pointer items-center">
						<input
							type="checkbox"
							bind:checked={caseSensitive}
							class="peer sr-only"
							disabled={isSaving}
						/>
						<div
							class="peer h-6 w-11 rounded-full bg-gray-300 peer-checked:bg-purple-600 peer-focus:ring-2 peer-focus:ring-purple-500 peer-focus:outline-none peer-disabled:opacity-50 after:absolute after:top-[2px] after:left-[2px] after:h-5 after:w-5 after:rounded-full after:border after:border-gray-300 after:bg-white after:transition-all after:content-[''] peer-checked:after:translate-x-full peer-checked:after:border-white dark:bg-gray-600 dark:peer-checked:bg-purple-500"
						></div>
					</label>
				</div>
				<p class="text-xs text-gray-500 dark:text-gray-400">
					{caseSensitive
						? '启用：ABC 和 abc 被视为不同的关键词'
						: '禁用：ABC 和 abc 被视为相同的关键词'}
				</p>

				<div
					class="rounded-lg border border-amber-200 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950"
				>
					<p class="text-sm font-medium text-amber-800 dark:text-amber-200">附加过滤条件</p>
					<div class="mt-3 grid gap-3 md:grid-cols-2">
						<div class="space-y-1">
							<label
								for="keyword-filter-min-duration"
								class="text-xs font-medium text-gray-700 dark:text-gray-300">最短时长（秒）</label
							>
							<input
								id="keyword-filter-min-duration"
								type="number"
								min="0"
								step="1"
								bind:value={minDurationSeconds}
								disabled={isSaving}
								placeholder="例如 60"
								class="w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm outline-none transition focus:border-amber-500 dark:border-gray-600 dark:bg-gray-900"
							/>
						</div>
						<div class="space-y-1">
							<label
								for="keyword-filter-max-duration"
								class="text-xs font-medium text-gray-700 dark:text-gray-300">最长时长（秒）</label
							>
							<input
								id="keyword-filter-max-duration"
								type="number"
								min="0"
								step="1"
								bind:value={maxDurationSeconds}
								disabled={isSaving}
								placeholder="例如 1800"
								class="w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm outline-none transition focus:border-amber-500 dark:border-gray-600 dark:bg-gray-900"
							/>
						</div>
						<div class="space-y-1">
							<label
								for="keyword-filter-published-after"
								class="text-xs font-medium text-gray-700 dark:text-gray-300">投稿起始日期</label
							>
							<input
								id="keyword-filter-published-after"
								type="date"
								bind:value={publishedAfter}
								disabled={isSaving}
								class="w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm outline-none transition focus:border-amber-500 dark:border-gray-600 dark:bg-gray-900"
							/>
						</div>
						<div class="space-y-1">
							<label
								for="keyword-filter-published-before"
								class="text-xs font-medium text-gray-700 dark:text-gray-300">投稿截止日期</label
							>
							<input
								id="keyword-filter-published-before"
								type="date"
								bind:value={publishedBefore}
								disabled={isSaving}
								class="w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm outline-none transition focus:border-amber-500 dark:border-gray-600 dark:bg-gray-900"
							/>
						</div>
					</div>
					<p class="mt-2 text-xs text-amber-700 dark:text-amber-300">
						留空表示不限制。投稿时间按自然日过滤，起止日期均包含当天。
					</p>
					{#if advancedValidationError}
						<p class="mt-2 text-xs text-red-500">{advancedValidationError}</p>
					{/if}
				</div>

				{#if isLoading}
					<div class="flex items-center justify-center py-8">
						<svg class="h-6 w-6 animate-spin text-purple-600" fill="none" viewBox="0 0 24 24">
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
						<span class="ml-2 text-sm text-gray-500">加载中...</span>
					</div>
				{:else}
					<!-- 标签页切换 -->
					<div class="flex border-b border-gray-200 dark:border-gray-700">
						<button
							type="button"
							class="flex items-center gap-2 px-4 py-2 text-sm font-medium transition-colors {activeTab ===
							'whitelist'
								? 'border-b-2 border-green-500 text-green-600 dark:text-green-400'
								: 'text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-300'}"
							on:click={() => (activeTab = 'whitelist')}
						>
							<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									stroke-width="2"
									d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
								/>
							</svg>
							白名单
							<span
								class="rounded-full bg-green-100 px-2 py-0.5 text-xs text-green-700 dark:bg-green-900 dark:text-green-300"
							>
								{whitelistKeywords.length}
							</span>
						</button>
						<button
							type="button"
							class="flex items-center gap-2 px-4 py-2 text-sm font-medium transition-colors {activeTab ===
							'blacklist'
								? 'border-b-2 border-red-500 text-red-600 dark:text-red-400'
								: 'text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-300'}"
							on:click={() => (activeTab = 'blacklist')}
						>
							<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
								<path
									stroke-linecap="round"
									stroke-linejoin="round"
									stroke-width="2"
									d="M18.364 18.364A9 9 0 005.636 5.636m12.728 12.728A9 9 0 015.636 5.636m12.728 12.728L5.636 5.636"
								/>
							</svg>
							黑名单
							<span
								class="rounded-full bg-red-100 px-2 py-0.5 text-xs text-red-700 dark:bg-red-900 dark:text-red-300"
							>
								{blacklistKeywords.length}
							</span>
						</button>
					</div>

					<!-- 白名单内容 -->
					{#if activeTab === 'whitelist'}
						<div
							class="space-y-3 rounded-lg border border-green-200 bg-green-50 p-4 dark:border-green-800 dark:bg-green-950"
						>
							<p class="text-sm text-green-700 dark:text-green-300">
								只下载匹配的视频（留空则不限制）
							</p>

							<!-- 添加白名单关键词 -->
							<div class="space-y-2">
								<div class="flex gap-2">
									<input
										type="text"
										bind:value={newWhitelistKeyword}
										on:keydown={handleWhitelistKeydown}
										placeholder="输入关键词或正则表达式"
										class="flex-1 rounded-md border border-green-300 px-3 py-2 text-sm focus:border-green-500 focus:ring-1 focus:ring-green-500 focus:outline-none dark:border-green-600 dark:bg-green-900 dark:text-green-100 dark:placeholder-green-400"
										disabled={isSaving}
									/>
									<button
										type="button"
										class="rounded-md bg-green-600 px-4 py-2 text-sm font-medium text-white hover:bg-green-700 focus:ring-2 focus:ring-green-500 focus:outline-none disabled:opacity-50"
										disabled={!newWhitelistKeyword.trim() || isSaving}
										on:click={addWhitelistKeyword}
									>
										添加
									</button>
								</div>
								{#if whitelistValidationError}
									<p class="text-xs text-red-500">{whitelistValidationError}</p>
								{/if}
							</div>

							<!-- 白名单列表 -->
							<div class="max-h-48 space-y-2 overflow-y-auto">
								{#if whitelistKeywords.length === 0}
									<p class="py-4 text-center text-sm text-green-600 italic dark:text-green-400">
										暂无白名单关键词
									</p>
								{:else}
									{#each whitelistKeywords as keyword, index}
										<div
											class="flex items-center justify-between rounded-md bg-green-100 px-3 py-2 dark:bg-green-900"
										>
											<code class="flex-1 text-sm break-all text-green-800 dark:text-green-200"
												>{keyword}</code
											>
											<button
												type="button"
												class="ml-2 flex-shrink-0 rounded p-1 text-green-600 hover:bg-green-200 hover:text-red-600 dark:hover:bg-green-800"
												disabled={isSaving}
												on:click={() => removeWhitelistKeyword(index)}
												title="删除"
											>
												<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
													<path
														stroke-linecap="round"
														stroke-linejoin="round"
														stroke-width="2"
														d="M6 18L18 6M6 6l12 12"
													/>
												</svg>
											</button>
										</div>
									{/each}
								{/if}
							</div>
						</div>
					{/if}

					<!-- 黑名单内容 -->
					{#if activeTab === 'blacklist'}
						<div
							class="space-y-3 rounded-lg border border-red-200 bg-red-50 p-4 dark:border-red-800 dark:bg-red-950"
						>
							<p class="text-sm text-red-700 dark:text-red-300">
								排除匹配的视频（优先级高于白名单）
							</p>

							<!-- 添加黑名单关键词 -->
							<div class="space-y-2">
								<div class="flex gap-2">
									<input
										type="text"
										bind:value={newBlacklistKeyword}
										on:keydown={handleBlacklistKeydown}
										placeholder="输入关键词或正则表达式"
										class="flex-1 rounded-md border border-red-300 px-3 py-2 text-sm focus:border-red-500 focus:ring-1 focus:ring-red-500 focus:outline-none dark:border-red-600 dark:bg-red-900 dark:text-red-100 dark:placeholder-red-400"
										disabled={isSaving}
									/>
									<button
										type="button"
										class="rounded-md bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-700 focus:ring-2 focus:ring-red-500 focus:outline-none disabled:opacity-50"
										disabled={!newBlacklistKeyword.trim() || isSaving}
										on:click={addBlacklistKeyword}
									>
										添加
									</button>
								</div>
								{#if blacklistValidationError}
									<p class="text-xs text-red-500">{blacklistValidationError}</p>
								{/if}
							</div>

							<!-- 黑名单列表 -->
							<div class="max-h-48 space-y-2 overflow-y-auto">
								{#if blacklistKeywords.length === 0}
									<p class="py-4 text-center text-sm text-red-600 italic dark:text-red-400">
										暂无黑名单关键词
									</p>
								{:else}
									{#each blacklistKeywords as keyword, index}
										<div
											class="flex items-center justify-between rounded-md bg-red-100 px-3 py-2 dark:bg-red-900"
										>
											<code class="flex-1 text-sm break-all text-red-800 dark:text-red-200"
												>{keyword}</code
											>
											<button
												type="button"
												class="ml-2 flex-shrink-0 rounded p-1 text-red-600 hover:bg-red-200 hover:text-red-800 dark:hover:bg-red-800"
												disabled={isSaving}
												on:click={() => removeBlacklistKeyword(index)}
												title="删除"
											>
												<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
													<path
														stroke-linecap="round"
														stroke-linejoin="round"
														stroke-width="2"
														d="M6 18L18 6M6 6l12 12"
													/>
												</svg>
											</button>
										</div>
									{/each}
								{/if}
							</div>
						</div>
					{/if}
				{/if}

				<!-- 正则表达式示例 -->
				<div
					class="rounded-lg border border-gray-200 bg-gray-50 p-3 dark:border-gray-700 dark:bg-gray-800"
				>
					<p class="text-xs font-medium text-gray-700 dark:text-gray-300">正则表达式示例：</p>
					<ul class="mt-1 space-y-1 text-xs text-gray-600 dark:text-gray-400">
						<li>
							<code class="rounded bg-gray-200 px-1 dark:bg-gray-600">PV</code> - 匹配包含"PV"的标题
						</li>
						<li>
							<code class="rounded bg-gray-200 px-1 dark:bg-gray-600">预告</code> - 匹配包含"预告"的标题
						</li>
						<li>
							<code class="rounded bg-gray-200 px-1 dark:bg-gray-600">第\d+期</code> - 匹配"第N期"格式
						</li>
						<li>
							<code class="rounded bg-gray-200 px-1 dark:bg-gray-600">^测试</code> - 匹配以"测试"开头的标题
						</li>
					</ul>
					<p class="mt-2 text-xs text-gray-500 dark:text-gray-400">
						示例：白名单添加"PV"，黑名单添加"预告"，则下载含"PV"但不含"预告"的视频
					</p>
				</div>
			</AlertDialog.Description>
		</AlertDialog.Header>
		<AlertDialog.Footer class="flex justify-end gap-3 pt-4">
			<button
				type="button"
				class="rounded-md border border-gray-300 bg-white px-4 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 focus:ring-2 focus:ring-indigo-500 focus:ring-offset-2 focus:outline-none disabled:cursor-not-allowed disabled:opacity-50 dark:border-gray-600 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600 dark:focus:ring-offset-gray-800"
				disabled={isSaving}
				on:click={handleCancel}
			>
				取消
			</button>
			<button
				type="button"
				class="rounded-md border border-transparent bg-purple-600 px-4 py-2 text-sm font-medium text-white hover:bg-purple-700 focus:ring-2 focus:ring-purple-500 focus:ring-offset-2 focus:outline-none disabled:cursor-not-allowed disabled:opacity-50"
				disabled={isSaving || isLoading}
				on:click={handleSave}
			>
				{#if isSaving}
					<svg class="mr-2 inline h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24">
						<circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"
						></circle>
						<path
							class="opacity-75"
							fill="currentColor"
							d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
						></path>
					</svg>
					保存中...
				{:else}
					保存
				{/if}
			</button>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
