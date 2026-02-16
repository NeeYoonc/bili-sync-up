<script lang="ts">
	import '../app.css';
	import AppSidebar from '$lib/components/app-sidebar.svelte';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import { goto } from '$app/navigation';
	import { Toaster } from '$lib/components/ui/sonner/index.js';
	import { breadcrumbStore } from '$lib/stores/breadcrumb';
	import BreadCrumb from '$lib/components/bread-crumb.svelte';
	import { videoSourceStore, setVideoSources } from '$lib/stores/video-source';
	import { onDestroy, onMount } from 'svelte';
	import api from '$lib/api';
	import { toast } from 'svelte-sonner';
	import type { ApiError, BetaImageUpdateStatusResponse } from '$lib/types';
	import { LogOut, BookOpen, ScrollText } from '@lucide/svelte';
	import ResponsiveButton from '$lib/components/responsive-button.svelte';
	import { initTheme } from '$lib/stores/theme';
	import ThemeToggle from '$lib/components/theme-toggle.svelte';
	import InstallPrompt from '$lib/components/pwa/install-prompt.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { APP_VERSION } from '$lib/generated/app-version';
	import { formatTimestamp } from '$lib/utils/timezone';
	import { installGlossaryTooltips } from '$lib/utils/glossary-tooltip';

	let dataLoaded = false;
	let isAuthenticated = false;
	let betaImageUpdateStatus: BetaImageUpdateStatusResponse | null = null;
	let showVersionTooltip = false;
	let versionBadgeContainer: HTMLDivElement | null = null;
	let cleanupGlossaryTooltips: (() => void) | null = null;

	// 退出登录
	function handleLogout() {
		api.setAuthToken('');
		isAuthenticated = false;
		goto('/');
		window.location.reload(); // 重新加载页面以清除状态
	}

	// 检查认证状态
	async function checkAuthStatus() {
		const token = localStorage.getItem('auth_token');
		if (token) {
			api.setAuthToken(token);
			try {
				// 验证token有效性
				await api.getVideoSources();
				isAuthenticated = true;
				checkBetaImageUpdateStatus();
				// 初始化视频源数据，所有组件都会用到
				if (!$videoSourceStore) {
					setVideoSources((await api.getVideoSources()).data);
				}
			} catch (error) {
				console.error('Token验证失败:', error);
				if ((error as ApiError).status === 401) {
					// Token 无效，清除
					isAuthenticated = false;
					api.setAuthToken('');
					localStorage.removeItem('auth_token');
				} else {
					// 只有在非401错误时才显示错误提示，避免新用户看到不必要的错误
					toast.error('加载视频来源失败', {
						description: (error as ApiError).message
					});
				}
			}
		} else {
			// 新用户没有token，这是正常情况，不需要显示错误
			isAuthenticated = false;
		}
		dataLoaded = true;
	}

	function getVersionBadgeTitle() {
		const base = `当前版本：${APP_VERSION}`;
		if (!betaImageUpdateStatus) {
			return base;
		}
		if (betaImageUpdateStatus.error) {
			return `${base}\n更新检查失败：${betaImageUpdateStatus.error}`;
		}

		function formatUpdateTime(ts?: string | null): string {
			if (!ts) return '未知';
			const formatted = formatTimestamp(ts, 'Asia/Shanghai', 'datetime');
			if (formatted === '无效时间' || formatted === '格式化失败') {
				return ts;
			}
			return formatted.replaceAll('/', '-');
		}

		const channel = betaImageUpdateStatus.release_channel ?? '未知';
		const channelLabel =
			channel === 'stable'
				? '正式版'
				: channel === 'beta'
					? '测试版'
					: channel === 'dev'
						? '开发版'
						: channel;

		const local = formatUpdateTime(betaImageUpdateStatus.local_built_at);
		const remote = formatUpdateTime(betaImageUpdateStatus.remote_pushed_at);
		const tag = betaImageUpdateStatus.checked_tag ?? '未知';
		const hint = betaImageUpdateStatus.update_available
			? `发现${channelLabel}镜像更新`
			: `${channelLabel}镜像已是最新`;
		return `${base}\n渠道：${channelLabel}（${tag}）\n${hint}\n当前版本构建时间：${local}\n仓库推送时间：${remote}`;
	}

	async function checkBetaImageUpdateStatus() {
		if (!isAuthenticated) return;

		try {
			const result = await api.getBetaImageUpdateStatus();
			betaImageUpdateStatus = result.data;
		} catch {
			// 静默失败，避免影响主流程
		}
	}

	function toggleVersionTooltip() {
		showVersionTooltip = !showVersionTooltip;
	}

	function handleDocumentClick(event: MouseEvent) {
		if (!showVersionTooltip) return;
		const target = event.target;
		if (!(target instanceof Node)) return;
		if (versionBadgeContainer && versionBadgeContainer.contains(target)) return;
		showVersionTooltip = false;
	}

	// Service Worker 更新提示
	function registerSWUpdateHandler() {
		if ('serviceWorker' in navigator) {
			navigator.serviceWorker.ready.then((registration) => {
				// 监听新的Service Worker进入waiting状态
				registration.addEventListener('updatefound', () => {
					const newWorker = registration.installing;
					if (newWorker) {
						newWorker.addEventListener('statechange', () => {
							if (newWorker.state === 'installed' && navigator.serviceWorker.controller) {
								// 新版本已准备好
								toast.info('发现新版本', {
									description: '点击更新以获取最新功能',
									duration: 10000,
									action: {
										label: '更新',
										onClick: () => {
											// 通知Service Worker跳过等待并激活
											newWorker.postMessage({ type: 'SKIP_WAITING' });
											// 刷新页面
											window.location.reload();
										}
									}
								});
							}
						});
					}
				});

				// 检查是否有更新
				registration.update().catch(() => {
					// 静默失败，不影响用户体验
				});
			});

			// 监听Service Worker控制器变化（新版本已激活）
			let refreshing = false;
			navigator.serviceWorker.addEventListener('controllerchange', () => {
				if (!refreshing) {
					refreshing = true;
					window.location.reload();
				}
			});
		}
	}

	// 初始化共用数据
	onMount(async () => {
		// 初始化主题
		initTheme();
		cleanupGlossaryTooltips = installGlossaryTooltips(document);
		// 注册Service Worker更新处理
		registerSWUpdateHandler();
		await checkAuthStatus();
		// 监听登录成功事件
		window.addEventListener('login-success', () => {
			isAuthenticated = true;
			checkAuthStatus();
		});
	});

	onMount(() => {
		document.addEventListener('click', handleDocumentClick);
	});

	onDestroy(() => {
		document.removeEventListener('click', handleDocumentClick);
		cleanupGlossaryTooltips?.();
		cleanupGlossaryTooltips = null;
	});
</script>

<Toaster />
<InstallPrompt />

<Sidebar.Provider>
	<div class="prevent-horizontal-scroll flex h-screen w-full overflow-hidden">
		{#if isAuthenticated}
			<div data-sidebar="sidebar">
				<AppSidebar />
			</div>
		{/if}
		<Sidebar.Inset class="flex h-screen flex-1 flex-col overflow-hidden">
			{#if isAuthenticated}
				<div
					class="bg-background/95 supports-[backdrop-filter]:bg-background/60 sticky top-0 z-50 flex min-h-[73px] w-full items-center border-b backdrop-blur"
				>
					<div class="flex w-full items-center gap-2 px-4 py-2 sm:gap-4 sm:px-6">
						<Sidebar.Trigger class="shrink-0" data-sidebar="trigger" />
						<div class="min-w-0 flex-1">
							<!-- 保留空间保持布局一致性 -->
						</div>
						<div class="flex items-center gap-1 sm:gap-2">
							<ThemeToggle />
							<div class="relative" bind:this={versionBadgeContainer}>
								<Badge
									variant="outline"
									class="max-w-[160px] cursor-pointer truncate font-mono text-[11px] select-none"
									title={getVersionBadgeTitle()}
									role="button"
									tabindex={0}
									onclick={toggleVersionTooltip}
									onkeydown={(e) => {
										if (e.key === 'Enter' || e.key === ' ') {
											e.preventDefault();
											toggleVersionTooltip();
										} else if (e.key === 'Escape') {
											showVersionTooltip = false;
										}
									}}
								>
									{APP_VERSION}
								</Badge>
								{#if showVersionTooltip}
									<div
										class="border-border bg-background fixed inset-x-4 top-[80px] z-50 max-h-[calc(100vh-100px)] overflow-auto rounded-lg border p-3 text-xs shadow-xl sm:absolute sm:inset-x-auto sm:top-full sm:right-0 sm:left-auto sm:mt-2 sm:max-h-[70vh] sm:w-[min(360px,calc(100vw-2rem))]"
									>
										<div class="leading-relaxed whitespace-pre-wrap">
											{getVersionBadgeTitle()}
										</div>
									</div>
								{/if}
								{#if betaImageUpdateStatus?.release_channel}
									<span
										class="border-border bg-background text-muted-foreground pointer-events-none absolute -right-1 -bottom-1 rounded border px-1 text-[10px] leading-[14px]"
									>
										{betaImageUpdateStatus.release_channel === 'stable'
											? '正'
											: betaImageUpdateStatus.release_channel === 'beta'
												? '测'
												: betaImageUpdateStatus.release_channel === 'dev'
													? '开'
													: betaImageUpdateStatus.release_channel}
									</span>
								{/if}
								{#if betaImageUpdateStatus?.update_available}
									<span
										class="bg-destructive border-background absolute -top-1 -right-1 h-2 w-2 rounded-full border-2"
										aria-label="发现更新"
									></span>
								{/if}
							</div>
							<ResponsiveButton
								size="sm"
								variant="outline"
								onclick={() => goto('/changelog')}
								icon={ScrollText}
								text="更新记录"
								showTextFrom="lg"
								title="查看更新记录"
							/>
							<ResponsiveButton
								size="sm"
								variant="outline"
								onclick={() => window.open('https://NeeYoonc.github.io/bili-sync-up/', '_blank')}
								icon={BookOpen}
								text="文档"
								showTextFrom="lg"
								title="查看文档"
							/>
							<ResponsiveButton
								size="sm"
								variant="outline"
								onclick={handleLogout}
								icon={LogOut}
								text="退出"
								showTextFrom="lg"
								title="退出"
							/>
						</div>
					</div>
				</div>
			{/if}
			<div class="bg-background smooth-scroll flex-1 overflow-auto">
				<div class="w-full px-4 py-4 sm:px-6 sm:py-6">
					{#if isAuthenticated && $breadcrumbStore.length > 0}
						<div class="mb-6">
							<BreadCrumb items={$breadcrumbStore} />
						</div>
					{/if}
					{#if dataLoaded}
						<slot />
					{/if}
				</div>
			</div>
		</Sidebar.Inset>
	</div>
</Sidebar.Provider>
