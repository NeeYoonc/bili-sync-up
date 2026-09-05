<script lang="ts">
	import { onMount } from 'svelte';
	import { setBreadcrumb } from '$lib/stores/breadcrumb';
	import * as Card from '$lib/components/ui/card';
	import { CHANGELOG_HTML, CHANGELOG_VERSION } from '$lib/generated/changelog';

	onMount(() => {
		setBreadcrumb([
			{ label: '总览', href: '/' },
			{ label: '更新记录', href: '/changelog' }
		]);
	});
</script>

<svelte:head>
	<title>更新记录 - Bili Sync-up</title>
</svelte:head>

<div class="space-y-6">
	<div class="flex items-start justify-between gap-4">
		<div>
			<h1 class="text-3xl font-bold tracking-tight">更新记录</h1>
			<p class="text-muted-foreground mt-2">查看 Bili Sync-up 的最新更新和改进记录</p>
		</div>
		<div class="text-right">
			<div class="text-muted-foreground text-xs">当前版本</div>
			<div class="font-mono text-sm">{CHANGELOG_VERSION}</div>
		</div>
	</div>

	<Card.Root class="min-h-[600px]">
		<Card.Content class="max-h-[calc(100vh-250px)] overflow-auto p-6">
			<article class="changelog-content prose prose-sm dark:prose-invert max-w-none">
				{@html CHANGELOG_HTML}
			</article>
		</Card.Content>
	</Card.Root>
</div>

<style>
	:global(.changelog-content h2) {
		margin-top: 2rem;
		margin-bottom: 0.75rem;
		padding: 0.6rem 0.9rem;
		border-radius: 0.65rem;
		border: 1px solid var(--border);
		border-left: 5px solid var(--primary);
		background: var(--muted);
		color: var(--foreground);
		font-size: 1.05rem;
		font-weight: 700;
		line-height: 1.4;
	}

	:global(.changelog-content h2:first-child) {
		margin-top: 0;
	}

	/* 每个版本内容块分隔 */
	:global(.changelog-content h2 + ul) {
		margin-top: 0.75rem;
		padding-bottom: 1.25rem;
		border-bottom: 1px solid var(--border);
	}

	:global(.changelog-content h2:last-of-type + ul) {
		padding-bottom: 0;
		border-bottom: none;
	}
</style>
