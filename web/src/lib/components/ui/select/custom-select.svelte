<script lang="ts" module>
	export type SelectValue = string | number | null;
	export type SelectOption = {
		value: SelectValue;
		label: string;
		disabled?: boolean;
		title?: string;
	};
</script>

<script lang="ts">
	import { onMount, tick } from 'svelte';
	import ChevronDownIcon from '@lucide/svelte/icons/chevron-down';
	import { cn } from '$lib/utils.js';

	export let id: string | undefined = undefined;
	export let value: SelectValue = null;
	export let options: SelectOption[] = [];
	export let placeholder = '请选择';
	export let title: string | undefined = undefined;
	export let disabled = false;
	export let clearAfterSelect = false;
	export let maxHeight = 280;
	export let onChange: ((value: SelectValue) => void) | undefined = undefined;
	export let className = '';
	export let contentClass = '';
	export let optionClass = '';
	export let size: 'sm' | 'default' = 'default';
	export { className as class };

	let open = false;
	let triggerEl: HTMLButtonElement | null = null;
	let listboxEl: HTMLDivElement | null = null;
	let activeIndex = -1;
	let top = 0;
	let width = 0;
	let currentMaxHeight = maxHeight;
	let generatedId = '';
	let dropdownReady = false;

	$: selectedIndex = options.findIndex((option) => option.value === value);
	$: selectedOption = selectedIndex >= 0 ? options[selectedIndex] : undefined;
	$: listboxId = id ? `${id}-listbox` : generatedId;
	$: activeOptionId = open && activeIndex >= 0 ? `${listboxId}-option-${activeIndex}` : undefined;
	$: contentStyle = `top: ${top}px; width: ${width}px; max-height: ${currentMaxHeight}px;`;
	$: wrapperClass = cn('relative inline-block align-middle w-full', extractWidthClasses(className));

	function extractWidthClasses(classes: string) {
		return classes
			.split(/\s+/)
			.filter((item) => /^(?:[a-z0-9_-]+:)*(?:w-|min-w-|max-w-)/.test(item))
			.join(' ');
	}

	function updatePosition() {
		if (!triggerEl) return false;
		const rect = triggerEl.getBoundingClientRect();
		const gap = 4;
		const viewportPadding = 8;
		const boundaryTop = viewportPadding;
		const boundaryBottom = window.innerHeight - viewportPadding;
		const spaceBelow = boundaryBottom - rect.bottom;
		const spaceAbove = rect.top - boundaryTop;
		const desiredHeight = Math.min(maxHeight, Math.max(80, options.length * 40 + 8));
		const placeAbove = spaceBelow < Math.min(desiredHeight, 180) && spaceAbove > spaceBelow;
		const availableHeight = Math.max(48, (placeAbove ? spaceAbove : spaceBelow) - gap);

		width = triggerEl.offsetWidth;
		currentMaxHeight = Math.min(maxHeight, availableHeight);
		top = placeAbove ? -(currentMaxHeight + gap) : triggerEl.offsetHeight + gap;
		return true;
	}

	function firstEnabledIndex(fromIndex: number, direction: 1 | -1) {
		if (options.length === 0) return -1;
		let index = fromIndex;
		for (let i = 0; i < options.length; i += 1) {
			index = (index + direction + options.length) % options.length;
			if (!options[index]?.disabled) return index;
		}
		return -1;
	}

	function scrollActiveOptionIntoView() {
		if (!listboxEl || activeIndex < 0) return;
		const optionEl = listboxEl.querySelector<HTMLElement>(`[data-index="${activeIndex}"]`);
		if (!optionEl) return;

		const optionTop = optionEl.offsetTop;
		const optionBottom = optionTop + optionEl.offsetHeight;
		const visibleTop = listboxEl.scrollTop;
		const visibleBottom = visibleTop + listboxEl.clientHeight;

		if (optionTop < visibleTop) {
			listboxEl.scrollTop = optionTop;
		} else if (optionBottom > visibleBottom) {
			listboxEl.scrollTop = optionBottom - listboxEl.clientHeight;
		}
	}

	async function setOpen(nextOpen: boolean) {
		if (disabled && nextOpen) return;
		if (!nextOpen) {
			open = false;
			dropdownReady = false;
			return;
		}

		activeIndex = selectedIndex >= 0 && !options[selectedIndex]?.disabled ? selectedIndex : firstEnabledIndex(-1, 1);
		dropdownReady = updatePosition();
		open = true;
		await tick();
		dropdownReady = updatePosition();
		scrollActiveOptionIntoView();
	}

	function handleTriggerClick() {
		setOpen(!open);
	}

	function selectOption(option: SelectOption) {
		if (option.disabled) return;
		const nextValue = option.value;
		value = clearAfterSelect ? null : nextValue;
		onChange?.(nextValue);
		setOpen(false);
		tick().then(() => triggerEl?.focus());
	}

	function moveActive(direction: 1 | -1) {
		const nextIndex = firstEnabledIndex(activeIndex, direction);
		if (nextIndex < 0) return;
		activeIndex = nextIndex;
		tick().then(() => {
			scrollActiveOptionIntoView();
		});
	}

	function handleKeydown(event: KeyboardEvent) {
		if (disabled) return;
		switch (event.key) {
			case 'ArrowDown':
				event.preventDefault();
				if (!open) {
					setOpen(true);
				} else {
					moveActive(1);
				}
				break;
			case 'ArrowUp':
				event.preventDefault();
				if (!open) {
					setOpen(true);
				} else {
					moveActive(-1);
				}
				break;
			case 'Enter':
			case ' ':
				event.preventDefault();
				if (!open) {
					setOpen(true);
				} else if (activeIndex >= 0 && options[activeIndex]) {
					selectOption(options[activeIndex]);
				}
				break;
			case 'Escape':
				if (open) {
					event.preventDefault();
					setOpen(false);
				}
				break;
			case 'Tab':
				if (open) setOpen(false);
				break;
		}
	}

	function handleDocumentPointerDown(event: PointerEvent) {
		if (!open) return;
		const target = event.target;
		if (!(target instanceof Node)) return;
		if (triggerEl?.contains(target) || listboxEl?.contains(target)) return;
		setOpen(false);
	}

	function handleViewportChange() {
		if (!open) return;
		updatePosition();
	}

	onMount(() => {
		generatedId = `custom-select-${Math.random().toString(36).slice(2)}`;
		document.addEventListener('pointerdown', handleDocumentPointerDown, true);
		window.addEventListener('resize', handleViewportChange);
		window.addEventListener('scroll', handleViewportChange, true);

		return () => {
			document.removeEventListener('pointerdown', handleDocumentPointerDown, true);
			window.removeEventListener('resize', handleViewportChange);
			window.removeEventListener('scroll', handleViewportChange, true);
		};
	});
</script>

<div class={wrapperClass}>
	<button
		bind:this={triggerEl}
		{id}
		type="button"
		role="combobox"
		aria-controls={listboxId}
		aria-expanded={open}
		aria-activedescendant={activeOptionId}
		aria-disabled={disabled}
		{title}
		{disabled}
		onclick={handleTriggerClick}
		onkeydown={handleKeydown}
		class={cn(
			'border-input bg-background text-foreground ring-offset-background focus-visible:ring-ring flex w-full items-center justify-between rounded-md border font-medium shadow-xs transition-[color,box-shadow] outline-none focus-visible:ring-2 focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50',
			size === 'sm' ? 'h-8 gap-2 px-2 py-1 text-xs' : 'h-10 gap-2 px-3 py-2 text-sm',
			className
		)}
	>
		<span class="truncate text-left">{selectedOption?.label ?? placeholder}</span>
		<ChevronDownIcon
			class={cn(
				'shrink-0 opacity-70 transition-transform',
				open && 'rotate-180',
				size === 'sm' ? 'h-3.5 w-3.5' : 'h-4 w-4'
			)}
		/>
	</button>

	{#if open}
		<div
			bind:this={listboxEl}
			id={listboxId}
			role="listbox"
			tabindex="-1"
			style={contentStyle}
			class={cn(
				'border-border bg-background text-foreground absolute left-0 z-[100] overflow-y-auto rounded-md border py-1 shadow-xl outline-none',
				dropdownReady ? 'animate-in fade-in-0 duration-75' : 'invisible',
				contentClass
			)}
			onkeydown={handleKeydown}
		>
			{#each options as option, index (`${option.value}-${index}`)}
				<button
					id={`${listboxId}-option-${index}`}
					type="button"
					role="option"
					data-index={index}
					aria-selected={option.value === value}
					disabled={option.disabled}
					title={option.title}
					onclick={() => selectOption(option)}
					onmouseenter={() => {
						if (!option.disabled) activeIndex = index;
					}}
					class={cn(
						'flex min-h-9 w-full items-center px-3 py-2 text-left text-sm transition-colors outline-none',
						option.disabled && 'text-muted-foreground cursor-not-allowed opacity-60',
						!option.disabled && index === activeIndex && 'bg-accent text-accent-foreground',
						!option.disabled && option.value === value && 'bg-primary text-primary-foreground',
						optionClass
					)}
				>
					<span class="truncate">{option.label}</span>
				</button>
			{/each}
		</div>
	{/if}
</div>
