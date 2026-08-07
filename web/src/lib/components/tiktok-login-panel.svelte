<script lang="ts">
	import { onMount } from 'svelte';
	import { toast } from 'svelte-sonner';
	import api from '$lib/api';
	import { Button } from '$lib/components/ui/button';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import type { TikTokStatusResponse } from '$lib/types';
	import {
		CheckCircle2,
		ChevronDown,
		ChevronRight,
		CircleAlert,
		Download,
		LoaderCircle,
		RefreshCw,
		Upload
	} from 'lucide-svelte';

	let status: TikTokStatusResponse | null = null;
	let loading = true;
	let importing = false;
	let collapsed = false;

	async function refresh() {
		loading = true;
		try {
			status = (await api.getTikTokStatus()).data;
		} catch (error) {
			toast.error('加载 TikTok 登录状态失败', {
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
			toast.success((await api.importTikTokCookies(await file.text())).data.message);
			await refresh();
		} catch (error) {
			toast.error('导入 TikTok cookies.txt 失败', {
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
	<CardHeader class="cursor-pointer" onclick={() => (collapsed = !collapsed)}>
		<CardTitle class="flex items-center gap-2">
			{#if collapsed}<ChevronRight class="h-4 w-4" />{:else}<ChevronDown class="h-4 w-4" />{/if}
			<span class="font-bold">🎵</span>
			TikTok 登录状态
			<Badge variant={status?.logged_in ? 'default' : 'secondary'} class="ml-auto">
				{status?.logged_in ? '已导入' : loading ? '检测中' : '未导入'}
			</Badge>
		</CardTitle>
	</CardHeader>
	{#if !collapsed}
		<CardContent>
			<div class="space-y-4 rounded-lg border p-3">
				<div class="flex flex-wrap items-center gap-3">
					<Badge variant={status?.logged_in ? 'default' : 'secondary'} class="gap-1">
						{#if status?.logged_in}<CheckCircle2 class="h-3.5 w-3.5" />{:else}<CircleAlert class="h-3.5 w-3.5" />{/if}
						{status?.logged_in ? 'TikTok Cookie 可用' : '未导入 TikTok Cookie'}
					</Badge>
					<a
						class="bg-primary text-primary-foreground hover:bg-primary/90 inline-flex h-9 items-center rounded-md px-3 text-sm font-medium"
						href="/youtube-login-helper.zip"
						download="bili-sync-login-helper.zip"
					>
						<Download class="mr-1 h-4 w-4" />下载电脑端登录助手
					</a>
					<label class="inline-flex">
						<input class="hidden" type="file" accept=".txt,text/plain" onchange={importCookieFile} disabled={importing} />
						<span class="border-input bg-background hover:bg-accent inline-flex h-9 cursor-pointer items-center rounded-md border px-3 text-sm font-medium">
							{#if importing}<LoaderCircle class="mr-1 h-4 w-4 animate-spin" />{:else}<Upload class="mr-1 h-4 w-4" />{/if}
							手动导入 cookies.txt
						</span>
					</label>
					<Button variant="ghost" size="sm" class="ml-auto" onclick={refresh}>
						<RefreshCw class="mr-1 h-4 w-4" />刷新状态
					</Button>
				</div>
				<div class="rounded-md border border-blue-300 bg-blue-50 p-3 text-sm text-blue-950 dark:border-blue-800 dark:bg-blue-950/30 dark:text-blue-100">
					<p class="font-medium">电脑端登录后直接传输</p>
					<ol class="mt-2 list-decimal space-y-1 pl-5">
						<li>加载登录助手并连接当前 Bili Sync 设置页。</li>
						<li>点击“打开 TikTok”，在同一电脑浏览器登录 TikTok。</li>
						<li>点击“传输 TikTok 登录状态”。Docker 会直接收到 Cookie；也可用 Cookie 扩展导出 cookies.txt 手动导入。</li>
					</ol>
				</div>
				<p class="text-muted-foreground text-xs">
					TikTok 喜欢列表等接口受一次性签名（X-Dynosaur）和风控保护，官方不开放公开调用；当前以作者主页同步为主。
					保存位置：<code class="break-all">{status?.cookie_path ?? '加载中…'}</code>
				</p>
			</div>
		</CardContent>
	{/if}
</Card>
