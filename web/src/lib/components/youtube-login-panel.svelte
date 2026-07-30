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
		Youtube
	} from 'lucide-svelte';

	let status: YouTubeStatusResponse | null = null;
	let browser: YouTubeBrowser = 'edge';
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

	async function startLogin() {
		importing = true;
		try {
			toast.info((await api.startYouTubeLogin(browser)).data.message);
		} catch (error) {
			toast.error('打开登录窗口失败', {
				description: error instanceof Error ? error.message : String(error)
			});
		} finally {
			importing = false;
		}
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
			<div class="flex flex-wrap items-center gap-3 rounded-lg border p-3">
				{#if status?.ytdlp_available}
					<Badge class="gap-1">
						<CheckCircle2 class="h-3.5 w-3.5" />yt-dlp {status.ytdlp_version}
					</Badge>
				{:else}
					<Badge variant="destructive" class="gap-1">
						<CircleAlert class="h-3.5 w-3.5" />{loading ? '检测中' : '未检测到 yt-dlp'}
					</Badge>
				{/if}
				<select
					class="border-input bg-background h-9 rounded-md border px-2 text-sm"
					bind:value={browser}
				>
					<option value="edge">Edge</option>
					<option value="chrome">Chrome</option>
					<option value="firefox">Firefox</option>
					<option value="brave">Brave</option>
					<option value="chromium">Chromium</option>
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
				<label class="inline-flex">
					<input
						class="hidden"
						type="file"
						accept=".txt,text/plain"
						onchange={importCookieFile}
						disabled={importing || !status?.ytdlp_available}
					/>
					<span
						class="bg-primary text-primary-foreground hover:bg-primary/90 inline-flex h-9 cursor-pointer items-center rounded-md px-3 text-sm font-medium"
					>
						导入 cookies.txt
					</span>
				</label>
				<Button variant="ghost" size="sm" class="ml-auto" onclick={refresh}>
					<RefreshCw class="mr-1 h-4 w-4" />刷新
				</Button>
			</div>
		</CardContent>
	{/if}
</Card>
