<script lang="ts">
	import { Button } from '$lib/components/ui/button';

	export let onclick: (() => void) | undefined = undefined;
	export let disabled = false;
	export let label = '全选';
	export let className = 'text-xs';
</script>

<Button size="sm" variant="outline" {disabled} {onclick} class={className}>
	{label}
</Button>
