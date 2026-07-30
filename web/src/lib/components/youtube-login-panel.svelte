<script lang="ts">
	import { onMount } from 'svelte';
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
		LoaderCircle,
		LogIn,
		RefreshCw,
		Upload,
		Youtube
	} from 'lucide-svelte';

	let status: YouTubeStatusResponse | null = null;
	let browser: YouTubeBrowser = 'edge';
	let loading = true;
	let importing = false;
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
			if (status?.sidecar_login_available) {
				window.open(resolveSidecarLoginUrl(), '_blank', 'noopener,noreferrer');
			}
			toast.info((await api.startYouTubeLogin(browser)).data.message);
		} catch (error) {
			toast.error('打开登录窗口失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			importing = false;
		}
	}

	function resolveSidecarLoginUrl() {
		const configured = status?.sidecar_login_url?.trim();
		if (configured && configured !== 'auto') {
			return configured;
		}
		const port = status?.sidecar_login_port ?? 3001;
		return `https://${window.location.hostname}:${port}`;
	}

	async function completeLogin() {
		importing = true;
		try {
			toast.success((await api.completeYouTubeLogin()).data.message);
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
					{#if status?.sidecar_login_available}
						<Badge variant="outline" class="gap-1 border-green-500 text-green-700">
							<CheckCircle2 class="h-3.5 w-3.5" />独立登录容器已连接
						</Badge>
						<Button size="sm" onclick={startLogin} disabled={importing || !status?.ytdlp_available}>
							{#if importing}<LoaderCircle class="mr-1 h-4 w-4 animate-spin" />{:else}<LogIn
									class="mr-1 h-4 w-4"
								/>{/if}
							打开 Docker 登录浏览器
						</Button>
						<Button
							size="sm"
							variant="outline"
							onclick={completeLogin}
							disabled={importing || !status?.ytdlp_available}
						>
							完成登录
						</Button>
					{:else if status?.browser_login_available}
						<select
							class="border-input bg-background h-9 rounded-md border px-2 text-sm"
							bind:value={browser}
						>
							{#each availableBrowsers as browserValue (browserValue)}
								<option value={browserValue}>{browserLabels[browserValue]}</option>
							{/each}
						</select>
						<Button size="sm" onclick={startLogin} disabled={importing || !status?.ytdlp_available}>
							{#if importing}<LoaderCircle class="mr-1 h-4 w-4 animate-spin" />{:else}<LogIn
									class="mr-1 h-4 w-4"
								/>{/if}
							打开登录窗口
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

				{#if status?.sidecar_login_available}
					<div
						class="rounded-md border border-green-300 bg-green-50 p-3 text-sm text-green-900 dark:border-green-800 dark:bg-green-950/30 dark:text-green-100"
					>
						<p class="font-medium">Docker 独立登录容器</p>
						<ol class="mt-2 list-decimal space-y-1 pl-5">
							<li>点击“打开 Docker 登录浏览器”，通过登录桌面的 HTTP Basic Auth。</li>
							<li>在 Chromium 中登录 YouTube，并确认首页右上角显示账号头像。</li>
							<li>回到本页点击“完成登录”，主程序会读取、验证并保存 YouTube Cookie。</li>
							<li>登录成功后可以停止 <code>youtube-login</code> 容器，不影响继续下载。</li>
						</ol>
						<p class="mt-2 text-xs opacity-80">
							浏览器位于独立容器，主 bili-sync 镜像不包含 Chromium、Xvfb 或 noVNC。
						</p>
					</div>
				{:else if status && !status.browser_login_available}
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
								{#if status.sidecar_login_configured}
									<div class="space-y-1">
										<p>启动或重建登录容器：</p>
										<code class="block overflow-x-auto rounded bg-black/5 p-2 text-xs dark:bg-white/10"
											>docker compose -f docker-compose.yml -f docker-compose.youtube-login.yml up
											-d</code
										>
										<p>启动后点击“刷新”，状态变为已连接即可登录。</p>
									</div>
								{:else}
									<ol class="list-decimal space-y-1 pl-5">
										<li>
											Docker 部署可添加 <code>docker-compose.youtube-login.yml</code> 独立登录容器。
										</li>
										<li>也可以在当前电脑浏览器登录 YouTube 并导出 Netscape cookies.txt。</li>
										<li>点击上方“从当前电脑导入 cookies.txt”，文件会上传并立即验证。</li>
									</ol>
								{/if}
								<p class="text-xs opacity-80">
									保存位置：<code class="break-all">{status.cookie_path}</code>
									{#if status.container_runtime}（请保持 Docker 配置目录卷映射）{/if}
								</p>
							</div>
						</div>
					</div>
				{:else if status?.browser_login_available}
					<p class="text-muted-foreground text-xs">
						本机浏览器登录和 cookies.txt 导入任选一种；不会把浏览器打包进主程序。
					</p>
				{/if}
			</div>
		</CardContent>
	{/if}
</Card>
