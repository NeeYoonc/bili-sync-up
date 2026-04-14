<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		title,
		description = '',
		titleTooltip = '',
		as = 'h3',
		titleClass = 'text-base font-semibold',
		descriptionClass = 'text-muted-foreground mt-1 text-sm',
		class: className = '',
		children,
		actions
	}: {
		title: string;
		description?: string;
		titleTooltip?: string;
		as?: 'h1' | 'h2' | 'h3' | 'h4' | 'div';
		titleClass?: string;
		descriptionClass?: string;
		class?: string;
		children?: Snippet;
		actions?: Snippet;
	} = $props();
</script>

<div class="flex flex-col gap-2 md:flex-row md:items-center md:justify-between {className}">
	<div class="min-w-0">
		<svelte:element this={as} class={titleClass} title={titleTooltip || undefined}
			>{title}</svelte:element
		>
		{#if description}
			<p class={descriptionClass}>{description}</p>
		{/if}
	</div>
	{#if actions || children}
		<div class="shrink-0 empty:hidden">
			{#if actions}
				{@render actions()}
			{:else if children}
				{@render children()}
			{/if}
		</div>
	{/if}
</div>
