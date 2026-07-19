<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { toast } from 'svelte-sonner';
	import { Button } from '$lib/components/ui/button';
	import { CustomSelect } from '$lib/components/ui/select';
	// import * as Tabs from '$lib/components/ui/tabs'; // 未使用，已注释
	import * as Card from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import { setBreadcrumb } from '$lib/stores/breadcrumb';
	import api from '$lib/api';
	import { runRequest } from '$lib/utils/request.js';
	import { IsMobile } from '$lib/hooks/is-mobile.svelte.js';
	import { RefreshCw, Download, AlertTriangle, XCircle, Info, Bug } from '@lucide/svelte';

	// 日志级别类型
	type LogLevel = 'info' | 'warn' | 'error' | 'debug';
	type LogFileLevel = LogLevel | 'all';

	// 日志条目类型
	interface LogEntry {
		timestamp: string;
		level: LogLevel;
		message: string;
		target?: string;
	}

	// 日志文件信息（每轮扫描会生成独立文件）
	interface LogFileInfo {
		level: LogFileLevel;
		file_name: string;
		size: number;
		modified: number; // Unix 时间戳（秒）
	}

	// 日志响应类型
	// interface LogsResponse { // 未使用，已注释
	// 	logs: LogEntry[];
	// 	total: number;
	// }

	// 响应式变量
	const isMobileQuery = new IsMobile();
	$: isMobile = isMobileQuery.current;

	// 状态变量
	let logs: LogEntry[] = [];
	let filteredLogs: LogEntry[] = [];
	let isLoading = false;
	let autoRefresh = true;
	let logFilesRefreshInterval: ReturnType<typeof setInterval> | null = null;
	let logsEventSource: EventSource | null = null;
	let liveStreamStatus: 'idle' | 'connecting' | 'connected' | 'error' = 'idle';
	let pendingLiveCount = 0;
	let pendingLiveLogs: LogEntry[] = [];
	let pendingLiveNeedsReload = false;
	let logsContainer: HTMLDivElement | null = null;
	let isLogsPaneHovered = false;
	let isLogsSelectionActive = false;
	let currentTab = 'all';
	let isAuthenticated = false;
	let authError = '';
	// let logLimit = 500; // 可自定义的日志数量限制 - 未使用，已注释
	let totalLogCount = 0; // 总日志数量
	let logFiles: LogFileInfo[] = [];
	let selectedLogFile = '';
	let isLoadingLogFiles = false;
	let logFilesError = '';

	// 分页相关变量
	let currentPage = 1;
	let totalPages = 0;
	let perPage = 100;

	// 日志级别颜色映射
	const levelColors: Record<LogLevel, string> = {
		info: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		warn: 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200',
		error: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200',
		debug: 'bg-gray-100 text-gray-800 dark:bg-gray-900 dark:text-gray-200'
	};

	// 日志级别图标映射
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	const levelIcons: Record<LogLevel, any> = {
		info: Info,
		warn: AlertTriangle,
		error: XCircle,
		debug: Bug
	};

	// 检查认证状态
	async function checkAuth(): Promise<boolean> {
		const token = localStorage.getItem('auth_token');
		if (!token) {
			authError = '未找到认证token，请先登录';
			return false;
		}

		const response = await runRequest(() => api.getVideoSources(), {
			context: '认证验证失败',
			showErrorToast: false,
			onError: () => {
				authError = '认证失败，请重新登录';
			}
		});
		return Boolean(response);
	}

	type ParsedLogsResponse = {
		logs: LogEntry[];
		total: number;
		page: number;
		per_page: number;
		total_pages: number;
	};

	function parseLogsResponse(
		result: unknown,
		fallback: { page: number; perPage: number }
	): ParsedLogsResponse {
		const defaultResponse: ParsedLogsResponse = {
			logs: [],
			total: 0,
			page: fallback.page,
			per_page: fallback.perPage,
			total_pages: 1
		};

		if (!result) return defaultResponse;
		if (Array.isArray(result)) {
			return {
				logs: result as LogEntry[],
				total: result.length,
				page: 1,
				per_page: result.length,
				total_pages: 1
			};
		}

		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		const obj = result as any;
		if (typeof obj !== 'object') return defaultResponse;

		if (obj.status_code === 200 && obj.data && typeof obj.data === 'object') {
			// eslint-disable-next-line @typescript-eslint/no-explicit-any
			const data = obj.data as any;
			const logs = Array.isArray(data.logs) ? (data.logs as LogEntry[]) : [];
			return {
				logs,
				total: typeof data.total === 'number' ? data.total : logs.length,
				page: typeof data.page === 'number' ? data.page : fallback.page,
				per_page: typeof data.per_page === 'number' ? data.per_page : fallback.perPage,
				total_pages: typeof data.total_pages === 'number' ? data.total_pages : 1
			};
		}

		if (obj.success && obj.data && typeof obj.data === 'object') {
			// eslint-disable-next-line @typescript-eslint/no-explicit-any
			const data = obj.data as any;
			const logs = Array.isArray(data.logs) ? (data.logs as LogEntry[]) : [];
			return {
				logs,
				total: typeof data.total === 'number' ? data.total : logs.length,
				page: typeof data.page === 'number' ? data.page : fallback.page,
				per_page: typeof data.per_page === 'number' ? data.per_page : fallback.perPage,
				total_pages: typeof data.total_pages === 'number' ? data.total_pages : 1
			};
		}

		if (Array.isArray(obj.logs)) {
			const logs = obj.logs as LogEntry[];
			return {
				logs,
				total: typeof obj.total === 'number' ? obj.total : logs.length,
				page: typeof obj.page === 'number' ? obj.page : fallback.page,
				per_page: typeof obj.per_page === 'number' ? obj.per_page : fallback.perPage,
				total_pages: typeof obj.total_pages === 'number' ? obj.total_pages : 1
			};
		}

		return defaultResponse;
	}

	function getCurrentLevelFilter(): LogLevel | undefined {
		return currentTab === 'all' ? undefined : (currentTab as LogLevel);
	}

	function buildLogsStreamUrl(level?: LogLevel): string | null {
		const token = localStorage.getItem('auth_token');
		if (!token) return null;

		const params = new URLSearchParams();
		params.append('token', token);
		if (level) {
			params.append('level', level);
		}
		return `/api/logs/stream?${params.toString()}`;
	}

	function stopLogsStream() {
		if (logsEventSource) {
			logsEventSource.close();
			logsEventSource = null;
		}
		liveStreamStatus = 'idle';
	}

	function hasSelectionInsideLogs(): boolean {
		if (!logsContainer) return false;
		const selection = window.getSelection();
		if (!selection || selection.isCollapsed || selection.rangeCount === 0) return false;

		const anchorNode = selection.anchorNode;
		const focusNode = selection.focusNode;
		return Boolean(
			(anchorNode && logsContainer.contains(anchorNode)) ||
				(focusNode && logsContainer.contains(focusNode))
		);
	}

	function shouldDeferLiveApply(): boolean {
		return isLogsPaneHovered || isLogsSelectionActive;
	}

	function flushPendingLiveLogs() {
		if (currentPage !== 1 || pendingLiveCount === 0) {
			return;
		}

		if (pendingLiveNeedsReload) {
			pendingLiveLogs = [];
			pendingLiveNeedsReload = false;
			void loadLogs(getCurrentLevelFilter(), 1);
			return;
		}

		if (pendingLiveLogs.length === 0) {
			pendingLiveCount = 0;
			return;
		}

		const bufferedLogs = [...pendingLiveLogs].reverse();
		logs = [...bufferedLogs, ...logs].slice(0, perPage);
		pendingLiveLogs = [];
		pendingLiveCount = 0;
		filterLogs();
	}

	function updateLogsSelectionState() {
		isLogsSelectionActive = hasSelectionInsideLogs();
		if (!shouldDeferLiveApply()) {
			flushPendingLiveLogs();
		}
	}

	function handleLogsMouseEnter() {
		isLogsPaneHovered = true;
	}

	function handleLogsMouseLeave() {
		isLogsPaneHovered = false;
		updateLogsSelectionState();
	}

	async function handlePendingLiveClick() {
		if (currentPage !== 1) {
			await loadLogs(getCurrentLevelFilter(), 1);
			return;
		}

		flushPendingLiveLogs();
	}

	function applyIncomingLog(entry: LogEntry) {
		if (currentPage === 1) {
			totalLogCount += 1;
			totalPages = Math.max(1, Math.ceil(totalLogCount / perPage));

			if (shouldDeferLiveApply()) {
				pendingLiveLogs.push(entry);
				pendingLiveCount += 1;
				return;
			}

			logs = [entry, ...logs].slice(0, perPage);
			filterLogs();
			return;
		}

		pendingLiveCount += 1;
		totalLogCount += 1;
		totalPages = Math.max(1, Math.ceil(totalLogCount / perPage));
	}

	function startLogsStream() {
		if (!autoRefresh || !isAuthenticated) return;

		const streamUrl = buildLogsStreamUrl(getCurrentLevelFilter());
		if (!streamUrl) return;

		stopLogsStream();
		liveStreamStatus = 'connecting';

		const eventSource = new EventSource(streamUrl);
		logsEventSource = eventSource;

		eventSource.addEventListener('ready', () => {
			liveStreamStatus = 'connected';
		});

		eventSource.addEventListener('log', (event) => {
			try {
				const entry = JSON.parse((event as MessageEvent).data) as LogEntry;
				applyIncomingLog(entry);
			} catch (error) {
				console.error('解析实时日志失败:', error);
			}
		});

		eventSource.addEventListener('lagged', () => {
			if (currentPage === 1) {
				if (shouldDeferLiveApply()) {
					pendingLiveNeedsReload = true;
					pendingLiveCount = Math.max(pendingLiveCount, 1);
					return;
				}
				void loadLogs(getCurrentLevelFilter(), 1);
				return;
			}
			pendingLiveCount = Math.max(pendingLiveCount, 1);
		});

		eventSource.onerror = () => {
			if (!autoRefresh) return;
			liveStreamStatus = 'error';
		};
	}

	function startAutoRefresh() {
		startLogsStream();
		if (!logFilesRefreshInterval) {
			logFilesRefreshInterval = setInterval(() => {
				void loadLogFiles();
			}, 15000);
		}
	}

	function stopAutoRefresh() {
		stopLogsStream();
		if (logFilesRefreshInterval) {
			clearInterval(logFilesRefreshInterval);
			logFilesRefreshInterval = null;
		}
	}

	function restartLogsStream() {
		if (!autoRefresh) return;
		startLogsStream();
	}

	// 初始化面包屑
	onMount(async () => {
		setBreadcrumb([
			{ label: '首页', href: '/' },
			{ label: '系统日志', href: '/logs' }
		]);

		// 验证认证状态
		isAuthenticated = await checkAuth();

		if (isAuthenticated) {
			// 加载日志（内存缓冲区）+ 日志文件列表（磁盘）
			void loadLogFiles();
			await loadLogs();

			// 设置自动刷新
			if (autoRefresh) {
				startAutoRefresh();
			}
		}

		document.addEventListener('selectionchange', updateLogsSelectionState);
	});

	onDestroy(() => {
		document.removeEventListener('selectionchange', updateLogsSelectionState);
		stopAutoRefresh();
	});

	// 加载日志
	async function loadLogs(level?: LogLevel, page: number = currentPage) {
		if (!isAuthenticated) {
			return;
		}

		authError = '';
		const params = new URLSearchParams();
		params.append('limit', perPage.toString());
		params.append('page', page.toString());

		if (level) {
			params.append('level', level);
		}

		const parsed = await runRequest(
			async () => {
				const token = localStorage.getItem('auth_token');
				if (!token) {
					throw new Error('未找到认证token');
				}

				const response = await fetch(`/api/logs?${params.toString()}`, {
					headers: {
						Authorization: token,
						'Content-Type': 'application/json'
					}
				});

				if (!response.ok) {
					if (response.status === 401) {
						isAuthenticated = false;
						authError = '认证失败，请重新登录';
						return null;
					}
					throw new Error(`HTTP ${response.status}: 加载日志失败`);
				}

				const result = await response.json();
				return parseLogsResponse(result, { page, perPage });
			},
			{
				setLoading: (value) => (isLoading = value),
				context: '加载日志失败',
				onError: (error) => {
					authError = error instanceof Error ? error.message : '加载日志失败';
				}
			}
		);
		if (!parsed) return;

		// 更新分页信息
		logs = parsed.logs;
		totalLogCount = parsed.total;
		currentPage = parsed.page;
		totalPages = parsed.total_pages;
		perPage = parsed.per_page;
		pendingLiveCount = 0;
		pendingLiveLogs = [];
		pendingLiveNeedsReload = false;

		filterLogs();
	}

	function getCurrentFileLevel(): LogFileLevel {
		const raw = currentTab === 'all' ? 'all' : (currentTab as LogLevel);
		return String(raw).trim().toLowerCase() as LogFileLevel;
	}

	let currentFileLevel: LogFileLevel = 'all';
	let logFilesForCurrentTab: LogFileInfo[] = [];
	let visibleLogFiles: LogFileInfo[] = [];
	$: currentFileLevel = getCurrentFileLevel();
	$: logFilesForCurrentTab = logFiles.filter(
		(f) => String(f.level).trim().toLowerCase() === currentFileLevel
	);
	// 如果当前 Tab 没有对应级别文件（例如只生成了 all/debug），则回退显示全部文件，避免“明明有数据却显示暂无”
	$: visibleLogFiles = logFilesForCurrentTab.length > 0 ? logFilesForCurrentTab : logFiles;

	$: {
		// 自动在切换 Tab 后选择“最新一轮”的同级别日志文件
		if (visibleLogFiles.length === 0) {
			selectedLogFile = '';
		} else if (
			!selectedLogFile ||
			!visibleLogFiles.some((f) => f.file_name === selectedLogFile)
		) {
			selectedLogFile = visibleLogFiles[0].file_name;
		}
	}

	// 根据当前选项卡过滤日志
	function filterLogs() {
		// 注意：现在我们使用服务器端过滤，这里主要用于显示
		// 实际的过滤在loadLogs函数中通过API参数完成
		filteredLogs = logs;
	}

	// 加载日志文件列表（每轮生成新文件）
	async function loadLogFiles() {
		if (!isAuthenticated) return;

		await runRequest(
			async () => {
				const token = localStorage.getItem('auth_token');
				if (!token) throw new Error('未找到认证token');

				const response = await fetch(`/api/logs/files`, {
					headers: {
						Authorization: token,
						'Content-Type': 'application/json'
					}
				});

				if (!response.ok) {
					if (response.status === 401) {
						isAuthenticated = false;
						authError = '认证失败，请重新登录';
						return;
					}
					if (response.status === 404) {
						logFilesError = '当前后端不支持日志文件列表（请升级后端）';
						toast.error(logFilesError);
						return;
					}
					throw new Error(`HTTP ${response.status}: 获取日志文件列表失败`);
				}

				// eslint-disable-next-line @typescript-eslint/no-explicit-any
				const result: any = await response.json();
				logFilesError = '';

				// 兼容不同的响应包装结构
				const files = Array.isArray(result?.data?.files)
					? result.data.files
					: Array.isArray(result?.files)
						? result.files
						: [];

				logFiles = files
					.map((f: any) => ({
						level: String(f.level ?? f.log_level ?? f.type ?? 'all')
							.trim()
							.toLowerCase() as LogFileLevel,
						file_name: String(f.file_name ?? f.fileName ?? f.filename ?? f.name ?? '').trim(),
						size: Number(f.size ?? f.file_size ?? f.length ?? 0),
						modified: Number(f.modified ?? f.mtime ?? f.updated_at ?? f.updatedAt ?? 0)
					}))
					.filter((f: LogFileInfo) => Boolean(f.file_name));
			},
			{
				setLoading: (value) => (isLoadingLogFiles = value),
				context: '获取日志文件列表失败',
				showErrorToast: false
			}
		);
	}

	// 分页相关函数
	function goToPage(page: number) {
		if (page >= 1 && page <= totalPages && page !== currentPage) {
			const level = currentTab === 'all' ? undefined : (currentTab as LogLevel);
			loadLogs(level, page);
		}
	}

	function goToFirstPage() {
		goToPage(1);
	}

	function goToLastPage() {
		goToPage(totalPages);
	}

	function goToPrevPage() {
		goToPage(currentPage - 1);
	}

	function goToNextPage() {
		goToPage(currentPage + 1);
	}

	// 手动刷新
	async function handleRefresh() {
		const level = currentTab === 'all' ? undefined : (currentTab as LogLevel);
		await Promise.all([loadLogs(level, currentPage), loadLogFiles()]);
	}

	// 切换自动刷新
	function toggleAutoRefresh() {
		autoRefresh = !autoRefresh;
		if (autoRefresh) {
			startAutoRefresh();
		} else {
			stopAutoRefresh();
		}
	}

	// 导出日志（从内存缓冲区）
	async function exportLogs() {
		if (!isAuthenticated) return;

		const maxExport = 10000;

		await runRequest(
			async () => {
				// 获取当前选择级别的所有日志
				const params = new URLSearchParams();
				// 后端最大支持 10000 条
				params.append('limit', String(maxExport));
				params.append('page', '1');

				if (currentTab !== 'all') {
					params.append('level', currentTab);
				}

				const token = localStorage.getItem('auth_token');
				if (!token) {
					throw new Error('未找到认证token');
				}

				const response = await fetch(`/api/logs?${params.toString()}`, {
					headers: {
						Authorization: token,
						'Content-Type': 'application/json'
					}
				});

				if (!response.ok) {
					throw new Error(`HTTP ${response.status}: 获取日志失败`);
				}

				const result = await response.json();
				const parsed = parseLogsResponse(result, { page: 1, perPage: maxExport });
				const allLogs = parsed.logs;

				if (allLogs.length === 0) {
					toast.error('没有日志可导出');
					return;
				}

				// 生成CSV内容
				const csvContent = [
					'时间,级别,消息,来源',
					...allLogs.map(
						(log) =>
							`"${formatTimestamp(log.timestamp)}","${log.level}","${log.message.replace(/"/g, '""')}","${log.target || ''}"`
					)
				].join('\n');

				// 创建文件名
				const levelText = currentTab === 'all' ? 'all' : currentTab;
				const fileName = `logs-buffer-${levelText}-${new Date().toISOString().split('T')[0]}.csv`;

				// 下载文件
				const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
				const link = document.createElement('a');
				link.href = URL.createObjectURL(blob);
				link.download = fileName;
				link.click();

				toast.success(`成功导出 ${allLogs.length} 条日志（内存缓冲区，最多 ${maxExport} 条）`);
			},
			{
				setLoading: (value) => (isLoading = value),
				context: '导出日志失败'
			}
		);
	}

	// 下载日志文件（从磁盘文件）
	async function downloadLogFile() {
		if (!isAuthenticated) return;
		await runRequest(
			async () => {
				// 一次性下载同一轮次的 5 份日志：all / info / warn / error / debug
				const baseFile = selectedLogFile || visibleLogFiles[0]?.file_name || '';
				const logId = baseFile ? getLogIdFromFileName(baseFile) : null;
				if (!logId) {
					throw new Error('未找到可用的日志文件（请先运行一轮任务）');
				}

				const bundle = [
					`logs-all-${logId}.csv`,
					`logs-info-${logId}.csv`,
					`logs-warn-${logId}.csv`,
					`logs-error-${logId}.csv`,
					`logs-debug-${logId}.csv`
				];

				let successCount = 0;
				for (const fileName of bundle) {
					// eslint-disable-next-line no-await-in-loop
					await downloadLogFileByName(fileName);
					successCount += 1;
					// 让浏览器有时间处理下载队列，降低被拦截概率
					// eslint-disable-next-line no-await-in-loop
					await new Promise((r) => setTimeout(r, 150));
				}

				toast.success(`已开始下载 ${successCount} 个日志文件（同一轮次）`);
			},
			{
				setLoading: (value) => (isLoading = value),
				context: '下载日志文件失败'
			}
		);
	}

	// 格式化时间戳
	function formatTimestamp(timestamp: string): string {
		return new Date(timestamp).toLocaleString('zh-CN', {
			year: 'numeric',
			month: '2-digit',
			day: '2-digit',
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function formatUnixSeconds(seconds: number): string {
		if (!seconds) return '-';
		return new Date(seconds * 1000).toLocaleString('zh-CN', {
			year: 'numeric',
			month: '2-digit',
			day: '2-digit',
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function formatBytes(bytes: number): string {
		if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		let value = bytes;
		let unitIndex = 0;
		while (value >= 1024 && unitIndex < units.length - 1) {
			value /= 1024;
			unitIndex += 1;
		}
		return `${value.toFixed(unitIndex === 0 ? 0 : 2)} ${units[unitIndex]}`;
	}

	function getLogIdFromFileName(fileName: string): string | null {
		const match = /^logs-(all|debug|info|warn|error)-(.+)\.csv$/i.exec(fileName.trim());
		return match?.[2] ?? null;
	}

	async function downloadLogFileByName(fileName: string) {
		const token = localStorage.getItem('auth_token');
		if (!token) throw new Error('未找到认证token');

		const params = new URLSearchParams();
		params.append('file', fileName);

		const response = await fetch(`/api/logs/download?${params.toString()}`, {
			headers: {
				Authorization: token
			}
		});

		if (!response.ok) {
			if (response.status === 401) {
				isAuthenticated = false;
				authError = '认证失败，请重新登录';
				return;
			}
			throw new Error(`HTTP ${response.status}: 下载日志文件失败 (${fileName})`);
		}

		const contentDisposition = response.headers.get('content-disposition');
		let downloadName = fileName;
		if (contentDisposition) {
			const matches = /filename="([^"]+)"/.exec(contentDisposition);
			if (matches) downloadName = matches[1];
		}

		const blob = await response.blob();
		const link = document.createElement('a');
		link.href = URL.createObjectURL(blob);
		link.download = downloadName;
		link.click();
		URL.revokeObjectURL(link.href);
	}

	// 重新登录
	function goToLogin() {
		window.location.href = '/';
	}
</script>

{#if !isAuthenticated}
	<!-- 未认证状态 -->
	<div class="container mx-auto py-12">
		<div class="text-center">
			<h1 class="mb-4 text-3xl font-bold">访问被拒绝</h1>
			<p class="text-muted-foreground mb-6">{authError}</p>
			<Button onclick={goToLogin}>返回登录</Button>
		</div>
	</div>
{:else}
	<!-- 已认证状态 - 显示日志界面 -->
	<div class="container mx-auto space-y-6">
		<!-- 页面标题和操作按钮 -->
		<div class="flex {isMobile ? 'flex-col gap-4' : 'items-center justify-between'}">
			<div>
				<div class="flex flex-wrap items-center gap-2">
					<h1 class="text-3xl font-bold tracking-tight">系统日志</h1>
					<Badge
						variant="outline"
						class={liveStreamStatus === 'connected'
							? 'border-green-200 bg-green-50 text-green-700'
							: liveStreamStatus === 'connecting'
								? 'border-blue-200 bg-blue-50 text-blue-700'
								: liveStreamStatus === 'error'
									? 'border-yellow-200 bg-yellow-50 text-yellow-700'
									: 'border-muted-foreground/20 text-muted-foreground'}
					>
						{#if liveStreamStatus === 'connected'}
							实时推送已连接
						{:else if liveStreamStatus === 'connecting'}
							实时推送连接中
						{:else if liveStreamStatus === 'error'}
							实时推送重连中
						{:else}
							实时推送未开启
						{/if}
					</Badge>
					{#if pendingLiveCount > 0}
						<Button variant="ghost" size="sm" class="h-7 px-2 text-xs" onclick={handlePendingLiveClick}>
							{#if currentPage === 1}
								当前有 {pendingLiveCount} 条新日志，点击更新查看
							{:else}
								当前页外有 {pendingLiveCount} 条新日志，点击跳到第一页查看
							{/if}
						</Button>
					{/if}
				</div>
				<p class="text-muted-foreground">查看系统运行日志和错误信息</p>
				{#if authError}
					<p class="mt-1 text-sm text-red-600">{authError}</p>
				{/if}
			</div>

			<div class="flex {isMobile ? 'flex-col' : ''} gap-2">
				<!-- 日志数量选择 -->
				<div class="flex items-center gap-2">
					<label for="perPage" class="text-sm font-medium whitespace-nowrap">每页显示:</label>
					<CustomSelect
						id="perPage"
						value={perPage}
						options={[
							{ value: 50, label: '50' },
							{ value: 100, label: '100' },
							{ value: 200, label: '200' },
							{ value: 500, label: '500' },
							{ value: 1000, label: '1000' },
							{ value: 5000, label: '5000' }
						]}
						onChange={(nextValue) => {
							perPage = Number(nextValue ?? 100);
							currentPage = 1;
							const level = currentTab === 'all' ? undefined : (currentTab as LogLevel);
							loadLogs(level, 1);
						}}
						size="sm"
						class="border-input bg-background h-8 rounded-md border px-2 py-1 text-sm"
					/>
				</div>

				<!-- 日志文件选择（每轮生成新文件） -->
				<div class="flex items-center gap-2">
					<label for="logFile" class="text-sm font-medium whitespace-nowrap">日志文件:</label>
					<CustomSelect
						id="logFile"
						value={selectedLogFile}
						options={visibleLogFiles.length === 0
							? [
									{
										value: '',
										label: logFilesError || '暂无可选日志文件（请先运行一轮任务）',
										disabled: true
									}
								]
							: visibleLogFiles.map((file) => ({
									value: file.file_name,
									label: `[${String(file.level).toUpperCase()}] ${formatUnixSeconds(
										file.modified
									)}（${formatBytes(file.size)}）`,
									title: file.file_name
								}))}
						onChange={(nextValue) => (selectedLogFile = String(nextValue ?? ''))}
						size="sm"
						class="border-input bg-background h-8 max-w-[260px] rounded-md border px-2 py-1 text-sm"
						disabled={isLoadingLogFiles || !isAuthenticated}
					/>
				</div>

				<Button variant="outline" size="sm" onclick={handleRefresh} disabled={isLoading}>
					<RefreshCw class="h-4 w-4 {isLoading ? 'animate-spin' : ''}" />
					刷新
				</Button>

				<Button
					variant="outline"
					size="sm"
					onclick={toggleAutoRefresh}
					class={autoRefresh
						? 'border-green-200 bg-green-50 text-green-700 hover:bg-green-100'
						: ''}
				>
					{autoRefresh ? '实时推送中' : '开启实时推送'}
				</Button>

				<Button
					variant="outline"
					size="sm"
					onclick={exportLogs}
					disabled={isLoading || !isAuthenticated}
					title="从内存缓冲区导出日志（最多10000条）"
				>
					<Download class="h-4 w-4" />
					导出日志
				</Button>

				<Button
					variant="outline"
					size="sm"
					onclick={downloadLogFile}
					disabled={isLoading || !isAuthenticated}
					title="从磁盘下载日志文件（同一轮次会下载 all/info/warn/error/debug 共5个文件）"
				>
					<Download class="h-4 w-4" />
					下载日志文件
				</Button>
			</div>
		</div>

		<!-- 日志选项卡 -->
		<div class="space-y-4">
			<!-- 选项卡按钮 -->
			<div class="bg-muted flex space-x-1 rounded-lg p-1">
				<button
					class="flex-1 rounded-md px-3 py-2 text-sm font-medium transition-colors {currentTab ===
					'all'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
					on:click={() => {
						currentTab = 'all';
						currentPage = 1;
						loadLogs();
						restartLogsStream();
					}}
				>
					全部日志
				</button>
				<button
					class="flex-1 rounded-md px-3 py-2 text-sm font-medium transition-colors {currentTab ===
					'info'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
					on:click={() => {
						currentTab = 'info';
						currentPage = 1;
						loadLogs('info', 1);
						restartLogsStream();
					}}
				>
					信息
				</button>
				<button
					class="flex-1 rounded-md px-3 py-2 text-sm font-medium transition-colors {currentTab ===
					'warn'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
					on:click={() => {
						currentTab = 'warn';
						currentPage = 1;
						loadLogs('warn', 1);
						restartLogsStream();
					}}
				>
					警告
				</button>
				<button
					class="flex-1 rounded-md px-3 py-2 text-sm font-medium transition-colors {currentTab ===
					'error'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
					on:click={() => {
						currentTab = 'error';
						currentPage = 1;
						loadLogs('error', 1);
						restartLogsStream();
					}}
				>
					错误
				</button>
				<button
					class="flex-1 rounded-md px-3 py-2 text-sm font-medium transition-colors {currentTab ===
					'debug'
						? 'bg-background text-foreground shadow-sm'
						: 'text-muted-foreground hover:text-foreground'}"
					on:click={() => {
						currentTab = 'debug';
						currentPage = 1;
						loadLogs('debug', 1);
						restartLogsStream();
					}}
				>
					调试
				</button>
			</div>

			<!-- 日志内容 -->
			<Card.Root>
				<Card.Header>
					<Card.Title class="flex items-center gap-2">
						日志列表
						{#if isLoading}
							<RefreshCw class="h-4 w-4 animate-spin" />
						{/if}
					</Card.Title>
					<Card.Description>
						显示 {filteredLogs.length} 条日志
						{#if totalLogCount > logs.length}
							(共 {totalLogCount} 条，当前加载 {logs.length} 条)
						{:else if totalLogCount > 0}
							(共 {totalLogCount} 条)
						{/if}
						{#if totalPages > 1}
							- 第 {currentPage} 页，共 {totalPages} 页
						{/if}
					</Card.Description>
				</Card.Header>
				<Card.Content class="p-0">
					<div
						bind:this={logsContainer}
						class="h-[600px] overflow-auto"
						role="log"
						aria-label="系统日志列表"
						aria-live="off"
						on:mouseenter={handleLogsMouseEnter}
						on:mouseleave={handleLogsMouseLeave}
					>
						{#if filteredLogs.length === 0}
							<div class="text-muted-foreground flex h-32 items-center justify-center">
								{isLoading ? '加载中...' : '暂无日志'}
							</div>
						{:else}
							<div class="space-y-1 p-4">
								{#each filteredLogs as log, index (index)}
									<div class="mb-3 border-b border-gray-100 pb-3 last:border-b-0">
										<div class="flex {isMobile ? 'flex-col gap-2' : 'items-start justify-between'}">
											<div class="flex flex-1 items-start gap-3">
												<svelte:component
													this={levelIcons[log.level]}
													class="mt-1 h-4 w-4 flex-shrink-0 text-{log.level === 'error'
														? 'red'
														: log.level === 'warn'
															? 'yellow'
															: log.level === 'info'
																? 'blue'
																: 'gray'}-500"
												/>
												<div class="min-w-0 flex-1">
													<div
														class="flex {isMobile ? 'flex-col gap-1' : 'items-center gap-2'} mb-1"
													>
														<Badge variant="outline" class={levelColors[log.level]}>
															{log.level.toUpperCase()}
														</Badge>
														{#if log.target}
															<span class="text-muted-foreground font-mono text-xs"
																>{log.target}</span
															>
														{/if}
													</div>
													<p class="text-sm break-words">{log.message}</p>
												</div>
											</div>
											<div
												class="text-muted-foreground font-mono text-xs {isMobile
													? 'text-left'
													: 'text-right'} flex-shrink-0"
											>
												{formatTimestamp(log.timestamp)}
											</div>
										</div>
									</div>
								{/each}
							</div>
						{/if}
					</div>

					<!-- 分页控件 -->
					{#if totalPages > 1}
						<div
							class="border-t px-4 py-3 {isMobile
								? 'space-y-3'
								: 'flex items-center justify-between gap-4'}"
						>
							<div
								class="text-muted-foreground {isMobile
									? 'flex flex-wrap items-center gap-x-2 gap-y-1 text-xs'
									: 'text-sm'}"
							>
								<span class="whitespace-nowrap">
									显示第 {(currentPage - 1) * perPage + 1} - {Math.min(
										currentPage * perPage,
										totalLogCount
									)} 条
								</span>
								<span class="whitespace-nowrap">共 {totalLogCount} 条</span>
							</div>
							<div class="flex flex-wrap items-center gap-2 {isMobile ? 'justify-start' : ''}">
								<Button
									variant="outline"
									size="sm"
									class="shrink-0"
									onclick={goToFirstPage}
									disabled={currentPage === 1 || isLoading}
								>
									首页
								</Button>
								<Button
									variant="outline"
									size="sm"
									class="shrink-0"
									onclick={goToPrevPage}
									disabled={currentPage === 1 || isLoading}
								>
									上一页
								</Button>

								<!-- 页码按钮 -->
								{#each Array.from({ length: Math.min(5, totalPages) }, (_, i) => {
									const startPage = Math.max(1, Math.min(currentPage - 2, totalPages - 4));
									return startPage + i;
								}) as pageNum (pageNum)}
									<Button
										variant={pageNum === currentPage ? 'default' : 'outline'}
										size="sm"
										class="shrink-0"
										onclick={() => goToPage(pageNum)}
										disabled={isLoading}
									>
										{pageNum}
									</Button>
								{/each}

								<Button
									variant="outline"
									size="sm"
									class="shrink-0"
									onclick={goToNextPage}
									disabled={currentPage === totalPages || isLoading}
								>
									下一页
								</Button>
								<Button
									variant="outline"
									size="sm"
									class="shrink-0"
									onclick={goToLastPage}
									disabled={currentPage === totalPages || isLoading}
								>
									末页
								</Button>
							</div>
						</div>
					{/if}
				</Card.Content>
			</Card.Root>
		</div>
	</div>
{/if}
