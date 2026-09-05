<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card/index.js';
	import { Progress } from '$lib/components/ui/progress/index.js';
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { Button } from '$lib/components/ui/button/index.js';
	import * as Chart from '$lib/components/ui/chart/index.js';
	import * as Dialog from '$lib/components/ui/dialog/index.js';
	import * as Tabs from '$lib/components/ui/tabs/index.js';
	import MyChartTooltip from '$lib/components/custom/my-chart-tooltip.svelte';
	import { curveMonotoneX } from 'd3-shape';
	import { BarChart, AreaChart } from 'layerchart';
	import { setBreadcrumb } from '$lib/stores/breadcrumb';
	import { toast } from 'svelte-sonner';
	import api from '$lib/api';
	import { wsManager } from '$lib/ws';
	import { runRequest } from '$lib/utils/request.js';
	import { formatTimestampOrFallback } from '$lib/utils/timezone';
	import type {
		DashBoardResponse,
		SysInfo,
		TaskStatus,
		TaskControlStatusResponse,
		LatestIngestItem,
		DownloadProgressItem
	} from '$lib/types';
	import AuthLogin from '$lib/components/auth-login.svelte';
	import InitialSetup from '$lib/components/initial-setup.svelte';
	import EmptyState from '$lib/components/empty-state.svelte';
	import Loading from '$lib/components/ui/Loading.svelte';

	// 图标导入
	import CloudDownloadIcon from '@lucide/svelte/icons/cloud-download';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import HeartIcon from '@lucide/svelte/icons/heart';
	import FolderIcon from '@lucide/svelte/icons/folder';
	import UserIcon from '@lucide/svelte/icons/user';
	import ClockIcon from '@lucide/svelte/icons/clock';
		import VideoIcon from '@lucide/svelte/icons/video';
	import TvIcon from '@lucide/svelte/icons/tv';
	import HardDriveIcon from '@lucide/svelte/icons/hard-drive';
	import CpuIcon from '@lucide/svelte/icons/cpu';
	import MemoryStickIcon from '@lucide/svelte/icons/memory-stick';
	import PlayIcon from '@lucide/svelte/icons/play';
	import CheckCircleIcon from '@lucide/svelte/icons/check-circle';
	import CalendarIcon from '@lucide/svelte/icons/calendar';
	import PauseIcon from '@lucide/svelte/icons/pause';
	import SettingsIcon from '@lucide/svelte/icons/settings';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import XCircleIcon from '@lucide/svelte/icons/x-circle';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import BellIcon from '@lucide/svelte/icons/bell';
	import ListVideoIcon from '@lucide/svelte/icons/list-video';
	import MoreHorizontalIcon from '@lucide/svelte/icons/more-horizontal';

	// 认证状态
	let isAuthenticated = false;
	let needsInitialSetup = false;
	let checkingSetup = true;

	let dashboardData: DashBoardResponse | null = null;
	let sysInfo: SysInfo | null = null;
	let taskStatus: TaskStatus | null = null;
	let taskControlStatus: TaskControlStatusResponse | null = null;
	let latestIngests: LatestIngestItem[] = [];
	let loading = false;
	let loadingTaskControl = false;
	let loadingLatestIngests = false;
	let loadingTaskRefresh = false;
	let showIngestSheet = false;
	let showDownloadsSheet = false;
	type IngestView = 'latest' | 'recent';
	let ingestView: IngestView = 'latest';
	type IngestPlatform = 'all' | 'bilibili' | 'youtube' | 'douyin' | 'tiktok';
	let ingestPlatform: IngestPlatform = 'all';
	type MonitoringPlatform = 'bilibili' | 'youtube' | 'douyin' | 'tiktok';
	let monitoringPlatform: MonitoringPlatform = 'bilibili';
	let unsubscribeSysInfo: (() => void) | null = null;
	let unsubscribeTasks: (() => void) | null = null;
	let unsubscribeDownloads: (() => void) | null = null;
	let downloads: DownloadProgressItem[] = [];

	function formatBytes(bytes: number): string {
		if (bytes === 0) return '0 B';
		const k = 1024;
		const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(k));
		return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
	}

	function formatCpu(cpu: number): string {
		return `${cpu.toFixed(1)}%`;
	}

	function formatSpeed(bps: number | null): string {
		if (!bps || bps <= 0) return '-';
		const mbps = bps / 1024 / 1024;
		if (mbps >= 1) return `${mbps.toFixed(2)} MB/s`;
		const kbps = bps / 1024;
		return `${kbps.toFixed(0)} KB/s`;
	}

	const BEIJING_TIMEZONE = 'Asia/Shanghai';

	// 统一按北京时间显示（24小时制）
	function formatTime(timeStr: string | null | undefined): string {
		return formatTimestampOrFallback(timeStr, BEIJING_TIMEZONE, 'time', timeStr ?? '-');
	}

	function formatChartTime(v: string | number): string {
		return formatTimestampOrFallback(v, BEIJING_TIMEZONE, 'time', `${v}`);
	}

	// 从路径提取番剧名称（备用方案，当 series_name 不可用时）
	function extractSeriesNameFromPath(path: string): string {
		if (!path) return '番剧';
		// 处理 Windows 和 Unix 路径分隔符
		const parts = path.replace(/\\/g, '/').split('/');
		// 返回最后一个非空的文件夹名
		for (let i = parts.length - 1; i >= 0; i--) {
			const part = parts[i].trim();
			if (part && !part.includes('.')) {
				return part;
			}
		}
		return '番剧';
	}

	// 获取显示用的系列名称（优先使用 series_name，否则从路径提取）
	function getDisplaySeriesName(item: LatestIngestItem): string {
		if (item.series_name) {
			return item.series_name;
		}
		return extractSeriesNameFromPath(item.path);
	}

	// 处理登录成功
	function handleLoginSuccess() {
		isAuthenticated = true;
		loadInitialData();
	}

	// 处理初始设置完成
	function handleSetupComplete() {
		needsInitialSetup = false;
		checkingSetup = true;
		checkInitialSetup().then(() => {
			if (isAuthenticated) {
				window.dispatchEvent(new CustomEvent('login-success'));
			}
		});
	}

	// 检查是否需要初始设置
	async function checkInitialSetup() {
		try {
			const storedToken = localStorage.getItem('auth_token');

			if (!storedToken) {
				try {
					const setupCheck = await api.checkInitialSetup();
					if (setupCheck.data.needs_setup) {
						needsInitialSetup = true;
					} else {
						needsInitialSetup = false;
						isAuthenticated = false;
					}
				} catch {
					console.log('无法检查后端状态，显示初始设置');
					needsInitialSetup = true;
				}
				checkingSetup = false;
				return;
			}

			api.setAuthToken(storedToken);
			try {
				await api.getVideoSources();
				isAuthenticated = true;
				loadInitialData();
			} catch {
				localStorage.removeItem('auth_token');
				api.setAuthToken('');

				try {
					const setupCheck = await api.checkInitialSetup();
					if (setupCheck.data.needs_setup) {
						needsInitialSetup = true;
					} else {
						needsInitialSetup = false;
						isAuthenticated = false;
					}
				} catch {
					needsInitialSetup = false;
					isAuthenticated = false;
				}
			}
		} catch (error) {
			console.error('检查初始设置失败:', error);
			needsInitialSetup = false;
			isAuthenticated = false;
		} finally {
			checkingSetup = false;
		}
	}

	async function loadDashboard() {
		const response = await runRequest(() => api.getDashboard(), {
			setLoading: (value) => (loading = value),
			context: '加载仪表盘数据失败'
		});
		if (!response) return;
		dashboardData = response.data;
	}

	function getIngestViewTitle(view: IngestView = ingestView): string {
		return view === 'latest' ? '最新入库' : '最近处理';
	}

	const INGEST_PLATFORM_LABEL: Record<IngestPlatform, string> = {
		all: '全部',
		bilibili: 'B站',
		youtube: 'YouTube',
		douyin: '抖音',
		tiktok: 'TikTok'
	};

	const INGEST_PLATFORM_TABS: Array<{ value: IngestPlatform; label: string }> = [
		{ value: 'all', label: '全部' },
		{ value: 'bilibili', label: 'B站' },
		{ value: 'youtube', label: 'YouTube' },
		{ value: 'douyin', label: '抖音' },
		{ value: 'tiktok', label: 'TikTok' }
	];

	async function loadIngests(
		view: IngestView = ingestView,
		platform: IngestPlatform = ingestPlatform
	) {
		ingestView = view;
		ingestPlatform = platform;
		const request =
			view === 'latest'
				? () => api.getLatestIngests(10, platform)
				: () => api.getRecentIngests(10, platform);
		const response = await runRequest(request, {
			setLoading: (value) => (loadingLatestIngests = value),
			context: `加载${getIngestViewTitle(view)}失败`
		});
		if (!response) return;
		latestIngests = response.data.items || [];
	}

	// 加载任务控制状态
	async function loadTaskControlStatus() {
		try {
			const response = await api.getTaskControlStatus();
			taskControlStatus = response.data;
		} catch (error) {
			console.error('获取任务控制状态失败:', error);
		}
	}

	// 暂停所有任务
	async function pauseAllTasks() {
		if (loadingTaskControl) return;

		const response = await runRequest(() => api.pauseScanning(), {
			setLoading: (value) => (loadingTaskControl = value),
			context: '暂停任务失败'
		});
		if (!response) return;

		if (response.data.success) {
			toast.success(response.data.message);
			await loadTaskControlStatus();
		} else {
			toast.error('暂停任务失败', { description: response.data.message });
		}
	}

	// 恢复所有任务
	async function resumeAllTasks() {
		if (loadingTaskControl) return;

		const response = await runRequest(() => api.resumeScanning(), {
			setLoading: (value) => (loadingTaskControl = value),
			context: '恢复任务失败'
		});
		if (!response) return;

		if (response.data.success) {
			toast.success(response.data.message);
			await loadTaskControlStatus();
		} else {
			toast.error('恢复任务失败', { description: response.data.message });
		}
	}

	// 任务刷新（触发立即扫描/下载，不等待下一次定时触发）
	async function refreshTasks() {
		if (loadingTaskRefresh) return;

		const response = await runRequest(() => api.refreshScanning(), {
			setLoading: (value) => (loadingTaskRefresh = value),
			context: '任务刷新失败'
		});
		if (!response) return;

		if (response.data.success) {
			toast.success(response.data.message);
			await loadTaskControlStatus();
			// 刷新后同步刷新首页数据
			await loadDashboard();
			await loadIngests();
		} else {
			toast.error('任务刷新失败', { description: response.data.message });
		}
	}

	async function loadInitialData() {
		// 加载任务控制状态
		await loadTaskControlStatus();
		// 加载仪表盘数据
		await loadDashboard();
		// 加载最新入库
		await loadIngests('latest');
		// 加载当前下载进度
		await loadDownloads();
	}

	// 下载进度百分比（0-100），总大小未知时返回 0
	function downloadPercent(item: DownloadProgressItem): number {
		if (item.total_bytes <= 0) return 0;
		return Math.min(100, Math.round((item.downloaded_bytes / item.total_bytes) * 100));
	}

	// 卡片只显示第一个正在下载的任务，其余点「⋯」在弹窗里看
	$: primaryDownload = downloads.length > 0 ? downloads[0] : null;

	// 剩余时间可读化
	function formatEta(seconds: number): string {
		if (seconds < 60) return `${seconds} 秒`;
		const minutes = Math.floor(seconds / 60);
		const secs = seconds % 60;
		if (minutes < 60) return `${minutes} 分${secs > 0 ? ` ${secs} 秒` : ''}`;
		const hours = Math.floor(minutes / 60);
		return `${hours} 小时${minutes % 60 > 0 ? ` ${minutes % 60} 分` : ''}`;
	}

	// 加载当前下载进度
	async function loadDownloads() {
		try {
			const response = await api.getDownloadsProgress();
			downloads = response.data || [];
		} catch (error) {
			console.error('加载下载进度失败:', error);
		}
	}

	onMount(() => {
		setBreadcrumb([{ label: '首页' }]);

		// 订阅WebSocket事件
		unsubscribeSysInfo = wsManager.subscribeToSysInfo((data) => {
			sysInfo = data;
		});
		unsubscribeTasks = wsManager.subscribeToTasks((data: TaskStatus) => {
			taskStatus = data;
		});
		unsubscribeDownloads = wsManager.subscribeToDownloads((data: DownloadProgressItem[]) => {
			downloads = data;
		});

		// 检查认证状态
		checkInitialSetup();

		// 连接WebSocket
		wsManager.connect().catch((error) => {
			console.error('WebSocket连接失败:', error);
		});
	});

	onDestroy(() => {
		if (unsubscribeSysInfo) {
			unsubscribeSysInfo();
			unsubscribeSysInfo = null;
		}
		if (unsubscribeTasks) {
			unsubscribeTasks();
			unsubscribeTasks = null;
		}
		if (unsubscribeDownloads) {
			unsubscribeDownloads();
			unsubscribeDownloads = null;
		}
	});

	let memoryHistory: Array<{ time: Date; used: number; process: number }> = [];
	let cpuHistory: Array<{ time: Date; used: number; process: number }> = [];

	$: if (sysInfo) {
		memoryHistory = [
			...memoryHistory.slice(-19),
			{
				time: new Date(),
				used: Math.min(Math.max(sysInfo.used_memory, 0), sysInfo.total_memory),
				process: Math.min(Math.max(sysInfo.process_memory, 0), sysInfo.total_memory)
			}
		];
		cpuHistory = [
			...cpuHistory.slice(-19),
			{
				time: new Date(),
				used: Math.min(Math.max(sysInfo.used_cpu, 0), 100),
				process: Math.min(Math.max(sysInfo.process_cpu, 0), 100)
			}
		];
	}

	// 计算磁盘使用率
	$: diskUsagePercent = sysInfo
		? ((sysInfo.total_disk - sysInfo.available_disk) / sysInfo.total_disk) * 100
		: 0;

	// 图表配置
	const videoChartConfig = {
		bilibili: {
			label: 'B站',
			color: 'var(--color-sky-600)'
		},
		youtube: {
			label: 'YouTube',
			color: 'var(--color-red-600)'
		},
		douyin: {
			label: '抖音',
			color: 'var(--color-purple-600)'
		},
		tiktok: {
			label: 'TikTok',
			color: 'var(--color-emerald-600)'
		}
	} satisfies Chart.ChartConfig;

	// 合并各平台近七日新增视频为多序列图表数据（按天对齐，缺失补 0）
	type DayMultiCount = {
		day: string;
		bilibili: number;
		youtube: number;
		douyin: number;
		tiktok: number;
	};
	let dayMultiCounts: DayMultiCount[] = [];
	$: if (dashboardData) {
		const sources = [
			dashboardData.videos_by_day,
			dashboardData.youtube_videos_by_day ?? [],
			dashboardData.douyin_videos_by_day ?? [],
			dashboardData.tiktok_videos_by_day ?? []
		];
		const dayCount = Math.max(...sources.map((list) => list.length), 0);
		dayMultiCounts = [];
		for (let i = 0; i < dayCount; i++) {
			dayMultiCounts.push({
				day:
					dashboardData.videos_by_day[i]?.day ??
					dashboardData.youtube_videos_by_day[i]?.day ??
					dashboardData.douyin_videos_by_day[i]?.day ??
					dashboardData.tiktok_videos_by_day[i]?.day ??
					'',
				bilibili: dashboardData.videos_by_day[i]?.cnt ?? 0,
				youtube: dashboardData.youtube_videos_by_day?.[i]?.cnt ?? 0,
				douyin: dashboardData.douyin_videos_by_day?.[i]?.cnt ?? 0,
				tiktok: dashboardData.tiktok_videos_by_day?.[i]?.cnt ?? 0
			});
		}
	}

	const memoryChartConfig = {
		used: {
			label: '整体占用',
			color: 'var(--color-slate-700)'
		},
		process: {
			label: '程序占用',
			color: 'var(--color-slate-950)'
		}
	} satisfies Chart.ChartConfig;

	const cpuChartConfig = {
		used: {
			label: '整体占用',
			color: 'var(--color-slate-700)'
		},
		process: {
			label: '程序占用',
			color: 'var(--color-slate-950)'
		}
	} satisfies Chart.ChartConfig;
</script>

<svelte:head>
	<title>首页 - Bili Sync-up</title>
</svelte:head>

{#if checkingSetup}
	<div class="bg-background flex min-h-screen items-center justify-center">
		<div class="text-center">
			<div class="text-foreground mb-4 text-lg">正在检查系统状态...</div>
			<div class="text-muted-foreground text-sm">请稍候</div>
		</div>
	</div>
{:else if needsInitialSetup}
	<InitialSetup on:setup-complete={handleSetupComplete} />
{:else if !isAuthenticated}
	<AuthLogin on:login-success={handleLoginSuccess} />
{:else}
	<div class="space-y-6">
		{#if loading}
			<Loading />
		{:else}
			<!-- 第一行：存储空间 + 当前监听 -->
			<div class="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
				<Card class="lg:col-span-1">
					<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
						<CardTitle class="text-sm font-medium" title="显示当前磁盘可用空间、总容量和已用比例">
							存储空间
						</CardTitle>
						<HardDriveIcon class="text-muted-foreground h-4 w-4" />
					</CardHeader>
					<CardContent>
						{#if sysInfo}
							<div class="space-y-2">
								<div class="flex items-center justify-between">
									<div class="text-2xl font-bold">{formatBytes(sysInfo.available_disk)} 可用</div>
									<div class="text-muted-foreground text-sm">
										共 {formatBytes(sysInfo.total_disk)}
									</div>
								</div>
								<Progress value={diskUsagePercent} class="h-2" />
								<div class="text-muted-foreground text-xs">
									已使用 {diskUsagePercent.toFixed(1)}% 的存储空间
								</div>
							</div>
						{:else}
							<Loading size="sm" align="start" textClass="text-sm" />
						{/if}
					</CardContent>
				</Card>
				<Card class="lg:col-span-2">
					<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
						<CardTitle
							class="text-sm font-medium"
							title="显示当前监听的 B站、YouTube 和抖音视频源状态、下次扫描时间和监听统计"
						>
							当前监听
						</CardTitle>
						<DatabaseIcon class="text-muted-foreground h-4 w-4" />
					</CardHeader>
					<CardContent>
						{#if dashboardData}
							<div class="space-y-4">
								<!-- 监听状态 -->
								<div class="flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
									<div class="flex items-center gap-2">
										{#if taskControlStatus && taskControlStatus.is_paused}
											<Badge variant="destructive">已停止</Badge>
										{:else if dashboardData.monitoring_status.is_scanning}
											<Badge>扫描中</Badge>
										{:else}
											<Badge variant="outline">等待中</Badge>
										{/if}
									</div>
									<div class="flex items-center gap-2">
										<div class="text-muted-foreground text-sm">下次扫描</div>
										<div class="text-sm font-medium">
											{formatTime(dashboardData.monitoring_status.next_scan_time)}
										</div>
										<Button
											size="sm"
											variant="outline"
											onclick={() => {
												loadDashboard();
												loadIngests();
											}}
											class="h-8"
											title="刷新首页数据"
										>
											<RefreshCwIcon class="h-4 w-4 lg:mr-2" />
											<span class="hidden lg:inline">刷新</span>
										</Button>
									</div>
								</div>

								<!-- 扫描摘要 -->
								<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
									<div class="flex items-center justify-between">
										<span class="text-sm">监听源</span>
										<Badge variant="outline">
											{dashboardData.monitoring_status.active_sources} / {dashboardData
												.monitoring_status.total_sources}
										</Badge>
									</div>
									<div class="flex items-center justify-between">
										<span class="text-sm">上次扫描</span>
										<span class="text-muted-foreground text-sm">
											{formatTime(dashboardData.monitoring_status.last_scan_time)}
										</span>
									</div>
									<div class="flex items-center justify-between">
										<span class="text-sm">未启用</span>
										<span class="text-muted-foreground text-sm">
											{dashboardData.monitoring_status.inactive_sources}
										</span>
									</div>
								</div>

								<!-- 按平台切换具体监听项统计 -->
								<Tabs.Root bind:value={monitoringPlatform}>
									<Tabs.List class="grid w-full max-w-lg grid-cols-4">
										<Tabs.Trigger value="bilibili">B 站视频源</Tabs.Trigger>
										<Tabs.Trigger value="youtube">YouTube 视频源</Tabs.Trigger>
										<Tabs.Trigger value="douyin">抖音视频源</Tabs.Trigger>
										<Tabs.Trigger value="tiktok">TikTok 视频源</Tabs.Trigger>
									</Tabs.List>

									{#if monitoringPlatform === 'bilibili'}
										<div class="mt-4 grid grid-cols-2 gap-4 lg:grid-cols-3">
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<HeartIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">收藏夹</span>
												</div>
												<Badge variant="outline"
													>{dashboardData.enabled_favorites} / {dashboardData.total_favorites}</Badge
												>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<FolderIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">合集 / 列表</span>
												</div>
												<Badge variant="outline"
													>{dashboardData.enabled_collections} / {dashboardData.total_collections}</Badge
												>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<UserIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">投稿</span>
												</div>
												<Badge variant="outline"
													>{dashboardData.enabled_submissions} / {dashboardData.total_submissions}</Badge
												>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<ClockIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">稍后再看</span>
												</div>
												<Badge variant="outline">
													{dashboardData.enable_watch_later
														? `启用 (${dashboardData.total_watch_later})`
														: `禁用 (${dashboardData.total_watch_later})`}
												</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<TvIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">番剧</span>
												</div>
												<Badge variant="outline"
													>{dashboardData.enabled_bangumi} / {dashboardData.total_bangumi}</Badge
												>
											</div>
										</div>
									{:else if monitoringPlatform === 'youtube'}
										<div class="mt-4">
											<div class="grid grid-cols-2 gap-4 lg:grid-cols-3">
												<div class="flex items-center justify-between">
													<div class="flex items-center gap-2">
														<BellIcon class="text-muted-foreground h-4 w-4" />
														<span class="text-sm">订阅动态</span>
													</div>
													<Badge variant="outline">
														{dashboardData.enabled_youtube_subscriptions} / {dashboardData.total_youtube_subscriptions}
													</Badge>
												</div>
												<div class="flex items-center justify-between">
													<div class="flex items-center gap-2">
														<UserIcon class="text-muted-foreground h-4 w-4" />
														<span class="text-sm">频道</span>
													</div>
													<Badge variant="outline">
														{dashboardData.enabled_youtube_channels} / {dashboardData.total_youtube_channels}
													</Badge>
												</div>
												<div class="flex items-center justify-between">
													<div class="flex items-center gap-2">
														<ListVideoIcon class="text-muted-foreground h-4 w-4" />
														<span class="text-sm">播放列表</span>
													</div>
													<Badge variant="outline">
														{dashboardData.enabled_youtube_playlists} / {dashboardData.total_youtube_playlists}
													</Badge>
												</div>
												<div class="flex items-center justify-between">
													<div class="flex items-center gap-2">
														<HeartIcon class="text-muted-foreground h-4 w-4" />
														<span class="text-sm">喜欢的视频</span>
													</div>
													<Badge variant="outline">
														{dashboardData.enabled_youtube_liked} / {dashboardData.total_youtube_liked}
													</Badge>
												</div>
												<div class="flex items-center justify-between">
													<div class="flex items-center gap-2">
														<ClockIcon class="text-muted-foreground h-4 w-4" />
														<span class="text-sm">稍后观看</span>
													</div>
													<Badge variant="outline">
														{dashboardData.enabled_youtube_watch_later} / {dashboardData.total_youtube_watch_later}
													</Badge>
												</div>
											</div>
										</div>
									{:else if monitoringPlatform === 'douyin'}
										<div class="mt-4 grid grid-cols-2 gap-4 lg:grid-cols-3">
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<UserIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">作者投稿</span>
												</div>
												<Badge variant="outline">
													{dashboardData.enabled_douyin_authors} / {dashboardData.total_douyin_authors}
												</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<HeartIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">我的喜欢</span>
												</div>
												<Badge variant="outline">{dashboardData.enabled_douyin_liked} / {dashboardData.total_douyin_liked}</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<FolderIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">收藏夹</span>
												</div>
												<Badge variant="outline">{dashboardData.enabled_douyin_collections} / {dashboardData.total_douyin_collections}</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<ClockIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">稍后再看</span>
												</div>
												<Badge variant="outline">{dashboardData.enabled_douyin_watch_later} / {dashboardData.total_douyin_watch_later}</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<TvIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">放映厅</span>
												</div>
												<Badge variant="outline">{dashboardData.enabled_douyin_theaters} / {dashboardData.total_douyin_theaters}</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<ListVideoIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">短剧</span>
												</div>
												<Badge variant="outline">{dashboardData.enabled_douyin_series} / {dashboardData.total_douyin_series}</Badge>
											</div>
										</div>
									{:else}
										<div class="mt-4 grid grid-cols-2 gap-4 lg:grid-cols-3">
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<UserIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">作者投稿</span>
												</div>
												<Badge variant="outline">
													{dashboardData.enabled_tiktok_authors} / {dashboardData.total_tiktok_authors}
												</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<HeartIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">我的喜欢</span>
												</div>
												<Badge variant="outline">
													{dashboardData.enabled_tiktok_liked} / {dashboardData.total_tiktok_liked}
												</Badge>
											</div>
											<div class="flex items-center justify-between">
												<div class="flex items-center gap-2">
													<FolderIcon class="text-muted-foreground h-4 w-4" />
													<span class="text-sm">收藏夹</span>
												</div>
												<Badge variant="outline">
													{dashboardData.enabled_tiktok_collections} / {dashboardData.total_tiktok_collections}
												</Badge>
											</div>
										</div>
{/if}
								</Tabs.Root>
							</div>
						{:else}
							<Loading size="sm" align="start" textClass="text-sm" />
						{/if}
					</CardContent>
				</Card>
			</div>

			<!-- 第二行：入库概览 + 下载任务状态 -->
			<div class="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
				<Card class="max-w-full overflow-hidden lg:col-span-2">
					<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
						<CardTitle class="text-sm font-medium" title="显示近七日新增视频统计">
							入库概览
						</CardTitle>
						<VideoIcon class="text-muted-foreground h-4 w-4" />
					</CardHeader>
					<CardContent>
						{#if dayMultiCounts.length > 0}
							<div class="mb-4 space-y-2">
								<div class="flex items-center justify-between text-sm">
									<span>近七日新增视频</span>
									<span class="font-medium">
										{dayMultiCounts.reduce(
											(sum, v) => sum + v.bilibili + v.youtube + v.douyin + v.tiktok,
											0
										)}{' '}
										个
									</span>
								</div>
								<div class="flex flex-wrap gap-3 text-xs">
									{#each ['bilibili', 'youtube', 'douyin', 'tiktok'] as key (key)}
										<span class="flex items-center gap-1">
											<span
												class="inline-block h-2 w-2 rounded-sm"
												style="background:{videoChartConfig[key as keyof typeof videoChartConfig].color}"
											></span>
											<span class="text-muted-foreground"
												>{videoChartConfig[key as keyof typeof videoChartConfig].label}</span
											>
										</span>
									{/each}
								</div>
							</div>
							<Chart.Container config={videoChartConfig} class="h-[200px] w-full">
								<BarChart
									data={dayMultiCounts}
									x="day"
									axis="x"
									series={[
										{
											key: 'bilibili',
											label: 'B站',
											color: videoChartConfig.bilibili.color
										},
										{
											key: 'youtube',
											label: 'YouTube',
											color: videoChartConfig.youtube.color
										},
										{
											key: 'douyin',
											label: '抖音',
											color: videoChartConfig.douyin.color
										},
										{
											key: 'tiktok',
											label: 'TikTok',
											color: videoChartConfig.tiktok.color
										}
									]}
									props={{
										bars: {
											stroke: 'none',
											rounded: 'all',
											radius: 6,
											initialHeight: 0
										},
										highlight: { area: { fill: 'none' } },
										xAxis: { format: () => '' }
									}}
								>
									{#snippet tooltip()}
										<MyChartTooltip indicator="line" />
									{/snippet}
								</BarChart>
							</Chart.Container>
						{:else}
							<div class="text-muted-foreground flex h-[200px] items-center justify-center text-sm">
								暂无视频统计数据
							</div>
						{/if}

						<div class="mt-6 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
							<span class="text-sm font-medium">入库记录</span>
							<div class="flex flex-wrap gap-2">
								<Button
									size="sm"
									variant="outline"
									onclick={() => {
										loadIngests('latest');
										showIngestSheet = true;
									}}
									class="h-8"
									title="查看最新入库记录"
								>
									<VideoIcon class="mr-2 h-4 w-4" />
									最新入库
								</Button>
								<Button
									size="sm"
									variant="outline"
									onclick={() => {
										loadIngests('recent');
										showIngestSheet = true;
									}}
									class="h-8"
									title="查看最近处理记录"
								>
									<ClockIcon class="mr-2 h-4 w-4" />
									最近处理
								</Button>
							</div>
						</div>
					</CardContent>
				</Card>
				<Card class="max-w-full md:col-span-1">
					<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
						<CardTitle class="text-sm font-medium" title="显示下载与扫描任务的运行状态和控制入口">
							下载任务状态
						</CardTitle>
						<CloudDownloadIcon class="text-muted-foreground h-4 w-4" />
					</CardHeader>
					<CardContent>
						{#if taskStatus}
							<div class="space-y-4">
								<div class="grid grid-cols-1 gap-6">
									<div class="mb-4 space-y-2">
										<div class="flex items-center justify-between text-sm">
											<span>当前任务状态</span>
											{#if taskControlStatus && taskControlStatus.is_paused}
												<Badge variant="destructive">已停止</Badge>
											{:else if taskStatus.is_running}
												<Badge>扫描中</Badge>
											{:else}
												<Badge variant="outline">等待中</Badge>
											{/if}
										</div>
									</div>
									<div class="flex items-center justify-between">
										<div class="flex items-center gap-2">
											<PlayIcon class="text-muted-foreground h-4 w-4" />
											<span class="text-sm">开始运行</span>
										</div>
										<span class="text-muted-foreground text-sm">
											{formatTime(taskStatus.last_run)}
										</span>
									</div>
									<div class="flex items-center justify-between">
										<div class="flex items-center gap-2">
											<CheckCircleIcon class="text-muted-foreground h-4 w-4" />
											<span class="text-sm">运行结束</span>
										</div>
										<span class="text-muted-foreground text-sm">
											{formatTime(taskStatus.last_finish)}
										</span>
									</div>
									<div class="flex items-center justify-between">
										<div class="flex items-center gap-2">
											<CalendarIcon class="text-muted-foreground h-4 w-4" />
											<span class="text-sm">下次运行</span>
										</div>
										<span class="text-muted-foreground text-sm">
											{formatTime(taskStatus.next_run)}
										</span>
									</div>
								</div>

																<!-- 正在下载实时进度 -->
								<div class="mt-4 space-y-2 border-t pt-3">
									<div class="flex items-center justify-between">
										<span class="text-sm">正在下载{downloads.length > 0 ? `（${downloads.length}）` : ''}</span>
										{#if downloads.length > 0}
										<Button
										size="sm"
										variant="ghost"
										class="h-6 w-6 p-0"
										title="查看全部下载"
										onclick={() => (showDownloadsSheet = true)}
										>
										<MoreHorizontalIcon class="h-4 w-4" />
										</Button>
										{/if}
									</div>
									{#if primaryDownload}
										<div class="hover:bg-muted/40 rounded-md border p-2 transition-colors">
										<div class="flex items-center justify-between gap-2">
										<span class="truncate text-xs font-medium" title={primaryDownload.title}>
										{primaryDownload.title}
										</span>
										{#if primaryDownload.phase}
										<Badge variant="secondary" class="shrink-0 px-1.5 py-0 text-[10px]">
										{primaryDownload.phase}
										</Badge>
										{/if}
										</div>
										<div class="mt-1.5 flex items-center gap-2">
										<Progress value={downloadPercent(primaryDownload)} class="h-1.5 flex-1" />
										<span class="text-muted-foreground w-10 shrink-0 text-right text-[10px]">
										{primaryDownload.total_bytes > 0 ? `${downloadPercent(primaryDownload)}%` : '--'}
										</span>
										</div>
										<div class="text-muted-foreground mt-1 flex flex-wrap items-center gap-x-2 text-[10px]">
										<span>{formatBytes(primaryDownload.downloaded_bytes)}{primaryDownload.total_bytes > 0 ? ` / ${formatBytes(primaryDownload.total_bytes)}` : ''}</span>
										{#if primaryDownload.speed_bps > 0}<span>{formatSpeed(primaryDownload.speed_bps)}</span>{/if}
										{#if primaryDownload.eta_seconds}<span>剩余约 {formatEta(primaryDownload.eta_seconds)}</span>{/if}
										</div>
										</div>
									{:else}
										<div class="text-muted-foreground text-xs">当前无下载任务</div>
									{/if}
								</div>

<!-- 任务控制按钮 -->
								{#if taskControlStatus}
									<div class="grid grid-cols-2 gap-2">
										<Button
											size="sm"
											variant={taskControlStatus.is_paused ? 'default' : 'destructive'}
											onclick={taskControlStatus.is_paused ? resumeAllTasks : pauseAllTasks}
											disabled={loadingTaskControl}
											class="w-full"
											title={taskControlStatus.is_paused
												? '恢复所有下载和扫描任务'
												: '停止所有下载和扫描任务'}
										>
											{#if loadingTaskControl}
												<SettingsIcon class="mr-2 h-4 w-4 animate-spin" />
												处理中...
											{:else if taskControlStatus.is_paused}
												<PlayIcon class="mr-2 h-4 w-4" />
												恢复任务
											{:else}
												<PauseIcon class="mr-2 h-4 w-4" />
												停止任务
											{/if}
										</Button>

										<Button
											size="sm"
											variant="outline"
											onclick={refreshTasks}
											disabled={loadingTaskRefresh}
											class="w-full"
											title="立即刷新任务（触发新一轮扫描/下载）"
										>
											{#if loadingTaskRefresh}
												<SettingsIcon class="mr-2 h-4 w-4 animate-spin" />
												刷新中...
											{:else}
												<RefreshCwIcon class="mr-2 h-4 w-4" />
												任务刷新
											{/if}
										</Button>
									</div>
								{/if}
							</div>
						{:else}
							<Loading size="sm" align="start" textClass="text-sm" />
						{/if}
					</CardContent>
				</Card>
			</div>

			<!-- 第三行：系统监控 -->
			<div class="grid gap-4 md:grid-cols-2">
				<!-- 内存使用情况 -->
				<Card class="overflow-hidden">
					<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
						<CardTitle class="text-sm font-medium" title="显示系统总内存、已用内存和进程内存变化">
							内存使用情况
						</CardTitle>
						<MemoryStickIcon class="text-muted-foreground h-4 w-4" />
					</CardHeader>
					<CardContent>
						{#if sysInfo}
							<div class="mb-4 space-y-2">
								<div class="flex items-center justify-between text-sm">
									<span>当前内存使用</span>
									<span class="font-medium"
										>{formatBytes(sysInfo.used_memory)} / {formatBytes(sysInfo.total_memory)}</span
									>
								</div>
							</div>
						{/if}
						{#if memoryHistory.length > 0}
							<div class="h-[150px] w-full overflow-hidden">
								<Chart.Container config={memoryChartConfig} class="h-full w-full">
									<AreaChart
										data={memoryHistory}
										x="time"
										axis="x"
										yDomain={sysInfo?.total_memory ? [0, sysInfo.total_memory] : undefined}
										series={[
											{
												key: 'used',
												label: memoryChartConfig.used.label,
												color: memoryChartConfig.used.color
											},
											{
												key: 'process',
												label: memoryChartConfig.process.label,
												color: memoryChartConfig.process.color
											}
										]}
										props={{
											area: {
												curve: curveMonotoneX,
												line: { class: 'stroke-1' },
												'fill-opacity': 0.4
											},
											xAxis: {
												format: () => ''
											},
											yAxis: {
												format: (v: number) => formatBytes(v)
											}
										}}
									>
										{#snippet tooltip()}
											<MyChartTooltip
												labelFormatter={(v: string | number) => {
													return formatChartTime(v);
												}}
												valueFormatter={(v: string | number) => {
													const num = typeof v === 'string' ? parseFloat(v) : v;
													return formatBytes(num);
												}}
												indicator="line"
											/>
										{/snippet}
									</AreaChart>
								</Chart.Container>
							</div>
						{:else}
							<div class="text-muted-foreground flex h-[150px] items-center justify-center text-sm">
								等待数据...
							</div>
						{/if}
					</CardContent>
				</Card>

				<Card class="overflow-hidden">
					<CardHeader class="flex flex-row items-center justify-between space-y-0 pb-2">
						<CardTitle class="text-sm font-medium" title="显示当前 CPU 使用率和最近一段时间的变化">
							CPU 使用情况
						</CardTitle>
						<CpuIcon class="text-muted-foreground h-4 w-4" />
					</CardHeader>
					<CardContent class="overflow-hidden">
						{#if sysInfo}
							<div class="mb-4 space-y-2">
								<div class="flex items-center justify-between text-sm">
									<span>当前 CPU 使用率</span>
									<span class="font-medium">{formatCpu(sysInfo.used_cpu)}</span>
								</div>
							</div>
						{/if}
						{#if cpuHistory.length > 0}
							<div class="h-[150px] w-full overflow-hidden">
								<Chart.Container config={cpuChartConfig} class="h-full w-full">
									<AreaChart
										data={cpuHistory}
										x="time"
										axis="x"
										yDomain={[0, 100]}
										series={[
											{
												key: 'used',
												label: cpuChartConfig.used.label,
												color: cpuChartConfig.used.color
											},
											{
												key: 'process',
												label: cpuChartConfig.process.label,
												color: cpuChartConfig.process.color
											}
										]}
										props={{
											area: {
												curve: curveMonotoneX,
												line: { class: 'stroke-1' },
												'fill-opacity': 0.4
											},
											xAxis: {
												format: () => ''
											},
											yAxis: {
												format: (v: number) => `${v}%`
											}
										}}
									>
										{#snippet tooltip()}
											<MyChartTooltip
												labelFormatter={(v: string | number) => {
													return formatChartTime(v);
												}}
												valueFormatter={(v: string | number) => {
													const num = typeof v === 'string' ? parseFloat(v) : v;
													return formatCpu(num);
												}}
												indicator="line"
											/>
										{/snippet}
									</AreaChart>
								</Chart.Container>
							</div>
						{:else}
							<div class="text-muted-foreground flex h-[150px] items-center justify-center text-sm">
								等待数据...
							</div>
						{/if}
					</CardContent>
				</Card>
			</div>
		{/if}
	</div>

	<!-- 入库记录 Dialog 弹窗 -->
	<Dialog.Root bind:open={showIngestSheet}>
		<Dialog.Content class="sm:max-w-2xl">
			<Dialog.Header>
				<Dialog.Title class="flex items-center justify-between pr-8">
					<span>{getIngestViewTitle()}</span>
					<Button
						size="sm"
						variant="ghost"
						onclick={() => loadIngests()}
						disabled={loadingLatestIngests}
						class="h-7 px-2"
						title="刷新"
					>
						{#if loadingLatestIngests}
							<SettingsIcon class="h-4 w-4 animate-spin" />
						{:else}
							<RefreshCwIcon class="h-4 w-4" />
						{/if}
					</Button>
				</Dialog.Title>
			</Dialog.Header>
			<div class="mt-3 flex flex-wrap gap-1.5">
				{#each INGEST_PLATFORM_TABS as tab (tab.value)}
					<Button
						size="sm"
						variant={ingestPlatform === tab.value ? 'default' : 'outline'}
						class="h-7 px-2.5 text-xs"
						disabled={loadingLatestIngests}
						onclick={() => loadIngests(ingestView, tab.value)}
					>
						{tab.label}
					</Button>
				{/each}
			</div>
			<div class="mt-2 max-h-[60vh] space-y-2 overflow-auto">
				{#if latestIngests.length === 0}
					<EmptyState title={`暂无${getIngestViewTitle()}记录`} class="border-0 bg-transparent py-8" />
				{:else}
					{#each latestIngests as item (item.platform + '-' + item.video_id)}
						<div class="hover:bg-muted/30 rounded-lg border p-3 transition-colors">
							<div class="flex items-start justify-between gap-3">
								<div class="min-w-0 flex-1">
									<div class="flex items-center gap-2">
										<div class="truncate font-medium" title={item.video_name}>
											{item.video_name}
										</div>
										<Badge variant="outline" class="shrink-0 px-1.5 py-0 text-[10px]">
											{INGEST_PLATFORM_LABEL[item.platform]}
										</Badge>
									</div>
									<div class="text-muted-foreground mt-1 flex flex-wrap items-center gap-2 text-xs">
										{#if item.upper_name && item.upper_name.trim() !== ''}
											<span>{item.upper_name}</span>
										{:else}
											<span class="text-primary/70">{getDisplaySeriesName(item)}</span>
										{/if}
										<span>·</span>
										<span>{item.ingested_at}</span>
										{#if item.download_speed_bps && item.download_speed_bps > 0}
											<span>·</span>
											<span>{formatSpeed(item.download_speed_bps)}</span>
										{/if}
									</div>
									<div class="text-muted-foreground mt-1 truncate text-xs" title={item.path}>
										📁 {item.path}
									</div>
								</div>
								<div class="shrink-0">
									{#if item.status === 'success'}
										<div class="flex items-center gap-1 text-xs text-emerald-600">
											<CheckCircleIcon class="h-4 w-4" />
											<span class="hidden sm:inline">成功</span>
										</div>
									{:else if item.status === 'deleted'}
										<div class="flex items-center gap-1 text-xs text-amber-600">
											<Trash2Icon class="h-4 w-4" />
											<span class="hidden sm:inline">已删除</span>
										</div>
									{:else if item.status === 'pending'}
										<div class="flex items-center gap-1 text-xs text-sky-600">
											<ClockIcon class="h-4 w-4" />
											<span class="hidden sm:inline">处理中</span>
										</div>
									{:else}
										<div class="flex items-center gap-1 text-xs text-rose-600">
											<XCircleIcon class="h-4 w-4" />
											<span class="hidden sm:inline">失败</span>
										</div>
									{/if}
								</div>
							</div>
						</div>
					{/each}
				{/if}
			</div>
		</Dialog.Content>
	</Dialog.Root>

	<!-- 全部下载 Dialog 弹窗 -->
	<Dialog.Root bind:open={showDownloadsSheet}>
		<Dialog.Content class="sm:max-w-2xl">
			<Dialog.Header>
				<Dialog.Title>正在下载（{downloads.length}）</Dialog.Title>
			</Dialog.Header>
			<div class="mt-3 max-h-[60vh] space-y-2 overflow-auto">
				{#if downloads.length === 0}
					<EmptyState title="暂无下载任务" class="border-0 bg-transparent py-8" />
				{:else}
					{#each downloads as item (item.key)}
						<div class="hover:bg-muted/30 rounded-lg border p-3 transition-colors">
							<div class="min-w-0 flex-1">
								<div class="flex items-center gap-2">
									<div class="truncate font-medium" title={item.title}>{item.title}</div>
									<Badge variant="outline" class="shrink-0 px-1.5 py-0 text-[10px]">
										{INGEST_PLATFORM_LABEL[item.platform]}
									</Badge>
									{#if item.phase}
										<Badge variant="secondary" class="shrink-0 px-1.5 py-0 text-[10px]">{item.phase}</Badge>
									{/if}
								</div>
								<div class="mt-2 flex items-center gap-2">
									<Progress value={downloadPercent(item)} class="h-2 flex-1" />
									<span class="text-muted-foreground w-12 shrink-0 text-right text-xs">
										{item.total_bytes > 0 ? `${downloadPercent(item)}%` : '--'}
									</span>
								</div>
								<div class="text-muted-foreground mt-1 flex flex-wrap items-center gap-x-2 text-xs">
									<span>{formatBytes(item.downloaded_bytes)}{item.total_bytes > 0 ? ` / ${formatBytes(item.total_bytes)}` : ''}</span>
									{#if item.speed_bps > 0}<span>{formatSpeed(item.speed_bps)}</span>{/if}
									{#if item.eta_seconds}<span>剩余约 {formatEta(item.eta_seconds)}</span>{/if}
									<span>·</span>
									<span>开始于 {item.started_at}</span>
								</div>
							</div>
						</div>
					{/each}
				{/if}
			</div>
		</Dialog.Content>
	</Dialog.Root>
{/if}
