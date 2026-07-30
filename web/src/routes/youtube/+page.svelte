<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	onMount(() => goto('/video-sources', { replaceState: true }));
</script>
<p class="text-muted-foreground p-6 text-sm">正在转到统一的视频源管理…</p>
