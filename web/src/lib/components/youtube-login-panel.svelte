<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import type RFB from '@novnc/novnc/lib/rfb.js';
	import { toast } from 'svelte-sonner';
	import api from '$lib/api';
	import { Button } from '$lib/components/ui/button';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import type { YouTubeBrowser, YouTubeStatusResponse } from '$lib/types';
	import {
		CheckCircle2,
		ChevronDown,
		ChevronRight,
		CircleAlert,
		Expand,
		LoaderCircle,
		LogIn,
		Monitor,
		RefreshCw,
		Upload,
		X,
		Youtube
	} from 'lucide-svelte';

	let status: YouTubeStatusResponse | null = null;
	let browser: YouTubeBrowser = 'edge';
	let loading = true;
	let importing = false;
	let containerBrowserOpen = false;
	let containerBrowserConnecting = false;
	let containerBrowserMessage = '';
	let containerBrowserElement: HTMLDivElement | undefined;
	let containerRfb: RFB | null = null;
	let loginCollapsed = false;
	const browserLabels: Record<YouTubeBrowser, string> = {
		edge: 'Edge',
		chrome: 'Chrome',
		firefox: 'Firefox',
		brave: 'Brave',
		chromium: 'Chromium'
	};
	$: availableBrowsers = status?.available_browsers ?? [];

	async function refresh() {
		loading = true;
		try {
			status = (await api.getYouTubeStatus()).data;
			if (status.available_browsers.length > 0 && !status.available_browsers.includes(browser)) {
				browser = status.available_browsers[0];
			}
		} catch (error) {
			toast.error('加载 YouTube 登录状态失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			loading = false;
		}
	}

	async function startLogin() {
		importing = true;
		try {
			toast.info((await api.startYouTubeLogin(browser)).data.message);
			if (status?.container_browser_available) {
				await openContainerBrowser();
			}
		} catch (error) {
			toast.error('打开登录窗口失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			importing = false;
		}
	}

	async function openContainerBrowser() {
		containerBrowserOpen = true;
		await tick();
		if (containerRfb || !containerBrowserElement) return;
		containerBrowserConnecting = true;
		containerBrowserMessage = '正在连接 Docker 内置浏览器…';
		try {
			const { default: RFBClient } = await import('@novnc/novnc/lib/rfb.js');
			const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
			const token = localStorage.getItem('auth_token') ?? '';
			const socketUrl = `${protocol}//${window.location.host}/api/youtube/container-browser/ws?token=${encodeURIComponent(token)}`;
			const rfb = new RFBClient(containerBrowserElement, socketUrl, { shared: true });
			rfb.scaleViewport = true;
			rfb.resizeSession = true;
			rfb.background = '#111827';
			rfb.addEventListener('connect', () => {
				containerBrowserConnecting = false;
				containerBrowserMessage = '已连接 Docker 内置 Chromium，请直接在下方完成 Google 登录';
				rfb.focus();
			});
			rfb.addEventListener('disconnect', (event: Event) => {
				containerRfb = null;
				containerBrowserConnecting = false;
				const clean = (event as Event & { detail?: { clean?: boolean } }).detail?.clean;
				containerBrowserMessage = clean
					? '容器浏览器连接已关闭'
					: '容器浏览器连接中断，请确认 Docker 镜像已更新后重试';
			});
			rfb.addEventListener('securityfailure', (event: Event) => {
				const reason = (event as Event & { detail?: { reason?: string } }).detail?.reason;
				containerBrowserMessage = reason || '容器浏览器安全握手失败';
			});
			containerRfb = rfb;
		} catch (error) {
			containerBrowserConnecting = false;
			containerBrowserMessage = error instanceof Error ? error.message : String(error);
			toast.error('连接 Docker 内置浏览器失败', { description: containerBrowserMessage });
		}
	}

	function closeContainerBrowser() {
		containerRfb?.disconnect();
		containerRfb = null;
		containerBrowserOpen = false;
		containerBrowserConnecting = false;
	}

	async function completeLogin() {
		importing = true;
		try {
			toast.success((await api.completeYouTubeLogin()).data.message);
			if (status?.container_browser_available) {
				closeContainerBrowser();
			}
			await refresh();
		} catch (error) {
			toast.error('完成登录失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			importing = false;
		}
	}

	async function importCookieFile(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		importing = true;
		try {
			toast.success((await api.importYouTubeCookies(await file.text())).data.message);
			await refresh();
		} catch (error) {
			toast.error('导入 cookies.txt 失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			input.value = '';
			importing = false;
		}
	}

	onMount(refresh);
	onDestroy(closeContainerBrowser);
</script>

<Card>
	<CardHeader class="cursor-pointer" onclick={() => (loginCollapsed = !loginCollapsed)}>
		<CardTitle class="flex items-center gap-2" title="展开或收起 YouTube 登录管理">
			{#if loginCollapsed}
				<ChevronRight class="text-muted-foreground h-4 w-4" />
			{:else}
				<ChevronDown class="text-muted-foreground h-4 w-4" />
			{/if}
			<Youtube class="h-5 w-5 text-red-600" />
			YouTube 登录
			<Badge variant={status?.logged_in ? 'default' : 'secondary'} class="ml-auto">
				{status?.logged_in ? '已登录' : '未登录'}
			</Badge>
		</CardTitle>
	</CardHeader>
	{#if !loginCollapsed}
		<CardContent>
			<div class="space-y-3 rounded-lg border p-3">
				<div class="flex flex-wrap items-center gap-3">
				{#if status?.ytdlp_available}
					<Badge class="gap-1">
						<CheckCircle2 class="h-3.5 w-3.5" />yt-dlp {status.ytdlp_version}
					</Badge>
				{:else}
					<Badge variant="destructive" class="gap-1">
						<CircleAlert class="h-3.5 w-3.5" />{loading ? '检测中' : '未检测到 yt-dlp'}
					</Badge>
				{/if}
					{#if status?.browser_login_available}
						{#if status.container_browser_available}
							<Badge variant="outline" class="gap-1">
								<Monitor class="h-3.5 w-3.5" />Docker Chromium
							</Badge>
						{:else}
							<select
								class="border-input bg-background h-9 rounded-md border px-2 text-sm"
								bind:value={browser}
							>
								{#each availableBrowsers as browserValue (browserValue)}
									<option value={browserValue}>{browserLabels[browserValue]}</option>
								{/each}
							</select>
						{/if}
						<Button size="sm" onclick={startLogin} disabled={importing || !status?.ytdlp_available}>
							{#if importing}<LoaderCircle class="mr-1 h-4 w-4 animate-spin" />{:else}<LogIn
									class="mr-1 h-4 w-4"
								/>{/if}
							{status.container_browser_available ? '打开 Docker 登录浏览器' : '打开登录窗口'}
						</Button>
						<Button
							size="sm"
							variant="outline"
							onclick={completeLogin}
							disabled={importing || !status?.ytdlp_available}
						>
							完成登录
						</Button>
					{/if}
				<label class="inline-flex">
					<input
						class="hidden"
						type="file"
						accept=".txt,text/plain"
						onchange={importCookieFile}
						disabled={importing || !status?.ytdlp_available}
					/>
					<span
						class="bg-primary text-primary-foreground hover:bg-primary/90 inline-flex h-9 cursor-pointer items-center rounded-md px-3 text-sm font-medium {importing ||
						!status?.ytdlp_available
							? 'pointer-events-none opacity-50'
							: ''}"
					>
						{#if importing}
							<LoaderCircle class="mr-1 h-4 w-4 animate-spin" />
						{:else}
							<Upload class="mr-1 h-4 w-4" />
						{/if}
						从当前电脑导入 cookies.txt
					</span>
				</label>
				<Button variant="ghost" size="sm" class="ml-auto" onclick={refresh}>
					<RefreshCw class="mr-1 h-4 w-4" />刷新
				</Button>
				</div>

				{#if status && !status.browser_login_available}
					<div
						class="rounded-md border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-800 dark:bg-amber-950/30 dark:text-amber-100"
					>
						<div class="flex items-start gap-2">
							<CircleAlert class="mt-0.5 h-4 w-4 shrink-0" />
							<div class="space-y-2">
								<p class="font-medium">
									{status.container_runtime ? 'Docker 登录方式' : '当前环境无法打开浏览器'}
								</p>
								<p>{status.browser_login_message}</p>
								<ol class="list-decimal space-y-1 pl-5">
									<li>在您当前电脑的浏览器中正常登录 YouTube。</li>
									<li>使用浏览器 Cookie 导出工具，导出 Netscape 格式的 cookies.txt。</li>
									<li>点击上方“从当前电脑导入 cookies.txt”，文件会上传到服务端并立即验证。</li>
								</ol>
								<p class="text-xs opacity-80">
									保存位置：<code class="break-all">{status.cookie_path}</code>
									{#if status.container_runtime}（请保持 Docker 配置目录卷映射）{/if}
								</p>
							</div>
						</div>
					</div>
				{:else if status?.browser_login_available}
					<p class="text-muted-foreground text-xs">
						{status.browser_login_message}
					</p>
				{/if}

				{#if status?.container_browser_available && containerBrowserOpen}
					<div class="overflow-hidden rounded-lg border bg-slate-950">
						<div
							class="flex items-center gap-2 border-b border-slate-700 bg-slate-900 px-3 py-2 text-xs text-slate-100"
						>
							{#if containerBrowserConnecting}
								<LoaderCircle class="h-4 w-4 animate-spin" />
							{:else}
								<Expand class="h-4 w-4" />
							{/if}
							<span class="flex-1">{containerBrowserMessage}</span>
							<Button
								size="icon"
								variant="ghost"
								class="h-7 w-7 text-slate-100 hover:bg-slate-700 hover:text-white"
								onclick={closeContainerBrowser}
								title="关闭浏览器画面"
							>
								<X class="h-4 w-4" />
							</Button>
						</div>
						<div
							bind:this={containerBrowserElement}
							class="h-[min(62vh,620px)] min-h-[420px] w-full bg-slate-950"
							aria-label="Docker 内置 Chromium 登录画面"
						></div>
					</div>
					<div class="rounded-md border border-blue-300 bg-blue-50 p-3 text-sm text-blue-900">
						直接在上方登录 Google/YouTube。确认 YouTube 右上角显示账号头像后，点击“完成登录”；
						登录资料会保存在 Docker 配置卷中，容器重建后仍可使用。
					</div>
				{/if}
			</div>
		</CardContent>
	{/if}
</Card>
