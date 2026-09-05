<script lang="ts">
	let {
		src = '',
		alt = '',
		placeholder = '无图片',
		loading = 'lazy',
		decoding = 'async',
		class: className = '',
		placeholderClass = ''
	}: {
		src?: string;
		alt?: string;
		placeholder?: string;
		loading?: 'eager' | 'lazy';
		decoding?: 'sync' | 'async' | 'auto';
		class?: string;
		placeholderClass?: string;
	} = $props();

	let hasError = $state(false);

	function normalizeImageUrl(url: string): string {
		if (!url) return '';

		if (url.startsWith('https://')) return url;
		if (url.startsWith('//')) return 'https:' + url;
		if (url.startsWith('http://')) return url.replace('http://', 'https://');
		if (!url.startsWith('http')) return 'https://' + url;

		return url;
	}

	function isYouTubeImageUrl(url: string): boolean {
		try {
			const hostname = new URL(url).hostname.toLowerCase();
			return ['ytimg.com', 'ggpht.com', 'googleusercontent.com', 'youtube.com', 'googlevideo.com'].some(
				(domain) => hostname === domain || hostname.endsWith(`.${domain}`)
			);
		} catch {
			return false;
		}
	}

	function resolveImageUrl(url: string): string {
		const normalized = normalizeImageUrl(url);
		if (!normalized || !isYouTubeImageUrl(normalized)) return normalized;
		// YouTube 图片也必须由后端使用 YouTube 专用代理请求，
		// 避免添加源页面的封面/头像绕过代理由浏览器直连。
		return `/api/proxy/image?url=${encodeURIComponent(normalized)}`;
	}

	let imageUrl = $derived(resolveImageUrl(src));

	$effect(() => {
		src;
		hasError = false;
	});
</script>

{#if imageUrl && !hasError}
	<img
		src={imageUrl}
		{alt}
		class={className}
		{loading}
		{decoding}
		crossorigin="anonymous"
		referrerpolicy="no-referrer"
		onerror={() => (hasError = true)}
	/>
{:else}
	<div
		class="bg-muted text-muted-foreground flex items-center justify-center text-xs {placeholderClass} {className}"
	>
		{placeholder}
	</div>
{/if}
