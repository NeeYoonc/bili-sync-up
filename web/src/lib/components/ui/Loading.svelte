<script lang="ts">
	let {
		text = '加载中...',
		size = 'default',
		inline = false,
		showSpinner = false,
		align = 'center',
		spinnerClass = '',
		textClass = '',
		class: className = ''
	}: {
		text?: string;
		size?: 'sm' | 'default' | 'lg';
		inline?: boolean;
		showSpinner?: boolean;
		align?: 'center' | 'start';
		spinnerClass?: string;
		textClass?: string;
		class?: string;
	} = $props();

	const wrapperClasses = {
		sm: inline ? 'text-sm' : 'py-8 text-sm',
		default: inline ? 'text-base' : 'py-12 text-base',
		lg: inline ? 'text-base' : 'py-16 text-base'
	} as const;

	const spinnerSizeClasses = {
		sm: 'h-4 w-4',
		default: 'h-6 w-6',
		lg: 'h-8 w-8'
	} as const;

	const alignClass = align === 'start' ? 'justify-start' : 'justify-center';
</script>

<div class={`flex items-center ${alignClass} ${wrapperClasses[size]} ${className}`}>
	{#if showSpinner}
		<svg
			class={`${spinnerSizeClasses[size]} animate-spin ${spinnerClass}`}
			fill="none"
			viewBox="0 0 24 24"
		>
			<circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"
			></circle>
			<path
				class="opacity-75"
				fill="currentColor"
				d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
			></path>
		</svg>
	{/if}
	<div class={`${showSpinner ? 'ml-2' : ''} text-muted-foreground ${textClass}`}>{text}</div>
</div>
