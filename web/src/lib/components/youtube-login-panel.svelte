<script lang="ts">
	import { onMount } from 'svelte';
	import { toast } from 'svelte-sonner';
	import api from '$lib/api';
	import { Button } from '$lib/components/ui/button';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import type { YouTubeStatusResponse } from '$lib/types';
	import {
		CheckCircle2,
		ChevronDown,
		ChevronRight,
		CircleAlert,
		Download,
		LoaderCircle,
		RefreshCw,
		Upload,
		Youtube
	} from 'lucide-svelte';

	let status: YouTubeStatusResponse | null = null;
	let loading = true;
	let importing = false;
	let loginCollapsed = false;

	async function refresh() {
		loading = true;
		try {
			status = (await api.getYouTubeStatus()).data;
		} catch (error) {
			toast.error('加载 YouTube 登录状态失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			loading = false;
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
		<CardTitle class="flex items-center gap-2" title="展开或收起 YouTube 登录状态管理">
			{#if loginCollapsed}
				<ChevronRight class="text-muted-foreground h-4 w-4" />
			{:else}
				<ChevronDown class="text-muted-foreground h-4 w-4" />
			{/if}
			<Youtube class="h-5 w-5 text-red-600" />
			YouTube 登录状态
			<Badge variant={status?.logged_in ? 'default' : 'secondary'} class="ml-auto">
				{status?.logged_in ? '已导入' : '未导入'}
			</Badge>
		</CardTitle>
	</CardHeader>
	{#if !loginCollapsed}
		<CardContent>
			<div class="space-y-4 rounded-lg border p-3">
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

					<a
						class="bg-primary text-primary-foreground hover:bg-primary/90 inline-flex h-9 items-center rounded-md px-3 text-sm font-medium"
						href="/youtube-login-helper.zip"
						download="youtube-login-helper.zip"
					>
						<Download class="mr-1 h-4 w-4" />
						下载电脑端登录助手
					</a>

					<label class="inline-flex">
						<input
							class="hidden"
							type="file"
							accept=".txt,text/plain"
							onchange={importCookieFile}
							disabled={importing || !status?.ytdlp_available}
						/>
						<span
							class="border-input bg-background hover:bg-accent hover:text-accent-foreground inline-flex h-9 cursor-pointer items-center rounded-md border px-3 text-sm font-medium {importing ||
							!status?.ytdlp_available
								? 'pointer-events-none opacity-50'
								: ''}"
						>
							{#if importing}
								<LoaderCircle class="mr-1 h-4 w-4 animate-spin" />
							{:else}
								<Upload class="mr-1 h-4 w-4" />
							{/if}
							手动导入 cookies.txt
						</span>
					</label>

					<Button variant="ghost" size="sm" class="ml-auto" onclick={refresh}>
						<RefreshCw class="mr-1 h-4 w-4" />刷新状态
					</Button>
				</div>

				<div
					class="rounded-md border border-blue-300 bg-blue-50 p-3 text-sm text-blue-950 dark:border-blue-800 dark:bg-blue-950/30 dark:text-blue-100"
				>
					<p class="font-medium">电脑端登录并传输到 Bili Sync</p>
					<ol class="mt-2 list-decimal space-y-1 pl-5">
						<li>下载并解压登录助手，在电脑端 Chrome 或 Edge 中加载该扩展。</li>
						<li>保持本设置页打开，点击助手的“连接当前页面”。</li>
						<li>点击助手的“打开 YouTube”，在电脑浏览器中正常登录。</li>
						<li>再次打开助手，点击“传输登录状态”；本页刷新后会显示“已导入”。</li>
					</ol>
					<p class="mt-2 text-xs opacity-80">
						助手只读取 youtube.com 和 google.com 中维持 YouTube 会话的 Cookie，并调用现有导入接口传输到当前
						Bili Sync；不会传输 Google 密码，也不需要 Docker 内嵌浏览器。
					</p>
				</div>

				<div class="text-muted-foreground flex items-start gap-2 text-xs">
					<CircleAlert class="mt-0.5 h-4 w-4 shrink-0" />
					<p>
						手动备用方式：使用浏览器 Cookie 导出工具导出 Netscape 格式 cookies.txt，再点击“手动导入
						cookies.txt”。登录凭证已保存到数据库，导入后自动维护、重启不丢失。
					</p>
				</div>
			</div>
		</CardContent>
	{/if}
</Card>
