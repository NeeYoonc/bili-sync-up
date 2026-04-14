<script lang="ts">
	import * as AlertDialog from '$lib/components/ui/alert-dialog';

	export let open: boolean = false;
	export let onOpenChange: (open: boolean) => void;
	export let title: string;
	export let description: string = '';
	export let titleTooltip: string = '';
	export let isMobile: boolean = false;
	export let backgroundImage: string = '';

	let className: string = '';
	export { className as class };
</script>

<AlertDialog.Root {open} {onOpenChange}>
	<AlertDialog.Content
		class="{isMobile
			? 'h-[90vh]'
			: 'h-[calc(100vh-2rem)] sm:max-w-5xl'} flex w-full flex-col gap-0 overflow-hidden p-0 [&>button]:hidden {className}"
	>
		{#if backgroundImage}
			<div class="absolute inset-0 z-0 overflow-hidden">
				<img
					src={backgroundImage}
					alt="背景"
					class="h-full w-full object-cover"
					style="opacity: 0.6; filter: contrast(1.1) brightness(0.9);"
					loading="lazy"
				/>
				<div class="from-background/85 to-background/50 absolute inset-0 bg-gradient-to-br"></div>
			</div>
		{/if}

		<AlertDialog.Header class="{isMobile ? 'border-b p-4' : 'border-b p-6'} relative z-10">
			<div class="flex flex-col gap-1.5 pr-8">
				<slot name="header">
					<AlertDialog.Title title={titleTooltip || undefined}>{title}</AlertDialog.Title>
					{#if description}
						<AlertDialog.Description>{description}</AlertDialog.Description>
					{/if}
				</slot>
			</div>
			<button
				onclick={() => onOpenChange(false)}
				class="ring-offset-background focus:ring-ring absolute top-2 right-2 rounded-sm p-1 opacity-70 transition-opacity hover:bg-gray-100 hover:opacity-100 focus:ring-2 focus:ring-offset-2 focus:outline-none disabled:pointer-events-none dark:hover:bg-gray-800"
				type="button"
			>
				<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						stroke-width="2"
						d="M6 18L18 6M6 6l12 12"
					/>
				</svg>
				<span class="sr-only">关闭</span>
			</button>
		</AlertDialog.Header>

		<div class="relative z-10 flex min-h-0 flex-1 flex-col">
			<slot />
		</div>
	</AlertDialog.Content>
</AlertDialog.Root>
