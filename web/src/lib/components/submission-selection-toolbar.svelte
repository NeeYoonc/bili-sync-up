<script lang="ts">
	import { Button } from '$lib/components/ui/button';

	export let query = '';
	export let placeholder = '搜索视频标题...';
	export let isSearching = false;
	export let statusText = '';
	export let compact = false;
	export let selectedCount = 0;
	export let totalCount = 0;
	export let onSelectAll: (() => void) | undefined = undefined;
	export let onSelectNone: (() => void) | undefined = undefined;
	export let onInvert: (() => void) | undefined = undefined;
	export let selectAllDisabled = false;
	export let selectNoneDisabled = false;
	export let invertDisabled = false;

	const inputClassBase =
		'w-full rounded-md border border-gray-300 pr-8 focus:border-blue-500 focus:ring-2 focus:ring-blue-500 focus:outline-none dark:border-gray-600 dark:bg-gray-700 dark:text-white';
	const buttonClassBase =
		'bg-card text-foreground hover:bg-muted rounded-md border border-gray-300 font-medium dark:border-gray-600';

	$: inputClass = compact
		? `${inputClassBase} px-2 py-1.5 text-xs sm:px-3 sm:py-2 sm:text-sm`
		: `${inputClassBase} px-3 py-2 text-sm`;
	$: buttonClass = compact
		? `${buttonClassBase} px-2 py-1 text-xs sm:px-3 sm:text-sm`
		: `${buttonClassBase} px-3 py-1 text-sm`;
	$: wrapperClass = compact
		? 'flex-shrink-0 space-y-2 px-1 sm:space-y-3'
		: 'flex-shrink-0 space-y-3 p-3';
	$: summaryClass = compact
		? 'text-muted-foreground text-xs sm:text-sm'
		: 'text-muted-foreground text-sm';
	$: controlsClass = compact
		? 'flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between'
		: 'flex items-center justify-between';
	$: actionGroupClass = compact ? 'flex gap-1 sm:gap-2' : 'flex gap-2';
</script>

<div class={wrapperClass}>
	<div class="flex gap-2">
		<div class="relative flex-1">
			<input
				type="text"
				bind:value={query}
				{placeholder}
				class={inputClass}
				disabled={isSearching}
			/>
			{#if isSearching}
				<div class="absolute inset-y-0 right-0 flex items-center pr-3">
					<svg class="h-4 w-4 animate-spin text-blue-600" fill="none" viewBox="0 0 24 24">
						<circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"
						></circle>
						<path
							class="opacity-75"
							fill="currentColor"
							d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
						></path>
					</svg>
				</div>
			{/if}
		</div>
	</div>

	{#if query.trim() && statusText}
		<div class="px-1 text-xs text-blue-600">{statusText}</div>
	{/if}

	<div class={controlsClass}>
		<div class={actionGroupClass}>
			<Button
				type="button"
				variant="ghost"
				class={buttonClass}
				onclick={onSelectAll}
				disabled={selectAllDisabled}>全选</Button
			>
			<Button
				type="button"
				variant="ghost"
				class={buttonClass}
				onclick={onSelectNone}
				disabled={selectNoneDisabled}>全不选</Button
			>
			<Button
				type="button"
				variant="ghost"
				class={buttonClass}
				onclick={onInvert}
				disabled={invertDisabled}>反选</Button
			>
		</div>

		<div class={summaryClass}>已选择 {selectedCount} / {totalCount} 个视频</div>
	</div>
</div>
