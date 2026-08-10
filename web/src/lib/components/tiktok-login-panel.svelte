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

	let manualSecUid = '';
	let manualSecUidLoaded = false;
	let savingSecUid = false;

	async function loadManualSecUid() {
		try {
			const data = (await api.getTikTokSecUid()).data;
			manualSecUid = data.manual_sec_uid ?? '';
		} catch (error) {
			toast.error('加载手动 secUid 失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			manualSecUidLoaded = true;
		}
	}

	async function saveManualSecUid() {
		savingSecUid = true;
		try {
			const result = (await api.setTikTokSecUid(manualSecUid.trim())).data;
			toast.success(result.message);
			await loadManualSecUid();
		} catch (error) {
			toast.error('保存手动 secUid 失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			savingSecUid = false;
		}
	}

	async function clearManualSecUid() {
		manualSecUid = '';
		await saveManualSecUid();
	}

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

	onMount(() => {
		refresh();
		loadManualSecUid();
	});
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
					<p class="font-medium">电脑端登录后直接传输（推荐）</p>
					<ol class="mt-2 list-decimal space-y-1 pl-5">
						<li>加载登录助手并连接当前 Bili Sync 设置页。</li>
						<li>点击“打开 TikTok”，在同一电脑浏览器登录 TikTok。</li>
						<li>点击“传输 TikTok 登录状态”，传输登录 Cookie（cookies.txt）。</li>
						<li>我的喜欢/关注列表仅需 cookies.txt 即可拉取；如返回空响应，请更换干净的出口 IP 或在设置页配置外源代理。</li>
					</ol>
				</div>
				<p class="text-muted-foreground text-xs">
					我的喜欢/关注列表仅需登录 Cookie（cookies.txt）即可服务端直连拉取；若返回空响应，通常是出口 IP 被 TikTok
					风控，请更换干净的出口 IP 或配置外源代理。Cookie 保存位置：
					<code class="break-all">{status?.cookie_path ?? '加载中…'}</code>
				</p>

				<div class="rounded-md border border-amber-300 bg-amber-50 p-3 text-sm text-amber-950 dark:border-amber-800 dark:bg-amber-950/30 dark:text-amber-100">
					<p class="font-medium">手动设置账号 secUid（服务端无法验证登录态时使用）</p>
					<p class="mt-1 text-xs">
						当服务端请求被 TikTok 风控、导入时提示“无法获取当前 TikTok 账号 secUid”时，可在已登录 TikTok 的浏览器控制台
						（F12 → Console）执行以下命令取得 secUid，然后粘贴到下方保存：
					</p>
					<code class="mt-1 block break-all rounded bg-black/10 p-2 font-mono text-xs dark:bg-white/10">
						fetch('https://www.tiktok.com/node-webapp/api/common-app-context?lang=zh-Hans').then(r=&gt;r.json()).then(d=&gt;console.log('secUid:', d.user?.secUid ?? '无'))
					</code>
					<div class="mt-2 flex flex-wrap items-center gap-2">
						<input
							class="border-input bg-background min-w-0 flex-1 rounded-md border px-3 py-2 font-mono text-xs"
							placeholder="MS4wLjABAAAA..."
							bind:value={manualSecUid}
							disabled={savingSecUid}
						/>
						<Button size="sm" variant="default" onclick={saveManualSecUid} disabled={savingSecUid || !manualSecUidLoaded}>
							{savingSecUid ? '保存中...' : '保存'}
						</Button>
						{#if manualSecUid}
							<Button size="sm" variant="outline" onclick={clearManualSecUid} disabled={savingSecUid}>
								清除
							</Button>
						{/if}
					</div>
					{#if manualSecUid}
						<p class="mt-2 text-xs">已保存手动 secUid：导入 cookies 或使用“我的喜欢/关注列表”时，服务端自动获取失败会回退使用该值。</p>
					{/if}
				</div>
			</div>
		</CardContent>
	{/if}
</Card>
