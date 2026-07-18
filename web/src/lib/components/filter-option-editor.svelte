<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Label } from '$lib/components/ui/label';
	import { createEventDispatcher } from 'svelte';
	import type { AudioQuality, FilterOption, VideoCodec, VideoQuality } from '$lib/types';

	export let value: FilterOption;
	export let disabled = false;

	const dispatch = createEventDispatcher<{ change: FilterOption }>();

	const videoQualityOptions: { value: VideoQuality; label: string }[] = [
		{ value: 'Quality8k', label: '8K超高清' },
		{ value: 'QualityDolby', label: '杜比视界' },
		{ value: 'QualityHdr', label: 'HDR真彩' },
		{ value: 'Quality4k', label: '4K超高清' },
		{ value: 'Quality1080p60', label: '1080P 60fps' },
		{ value: 'Quality1080pPLUS', label: '1080P+高码率' },
		{ value: 'Quality1080p', label: '1080P高清' },
		{ value: 'Quality720p', label: '720P高清' },
		{ value: 'Quality480p', label: '480P清晰' },
		{ value: 'Quality360p', label: '360P流畅' }
	];

	const audioQualityOptions: { value: AudioQuality; label: string }[] = [
		{ value: 'QualityHiRES', label: 'Hi-Res无损' },
		{ value: 'Quality192k', label: '192K高品质' },
		{ value: 'QualityDolby', label: '杜比全景声' },
		{ value: 'QualityDolbyBangumi', label: '杜比全景声（番剧）' },
		{ value: 'Quality132k', label: '132K标准' },
		{ value: 'Quality64k', label: '64K省流' }
	];

	const codecOptions: { value: VideoCodec; label: string }[] = [
		{ value: 'AVC', label: 'AVC/H.264' },
		{ value: 'HEV', label: 'HEVC/H.265' },
		{ value: 'AV1', label: 'AV1' }
	];

	function updateField<K extends keyof FilterOption>(key: K, nextValue: FilterOption[K]) {
		dispatch('change', { ...value, [key]: nextValue });
	}

	function updateBoolean(key: 'no_dolby_video' | 'no_dolby_audio' | 'no_hdr' | 'no_hires', checked: boolean) {
		updateField(key, checked);
	}

	function moveCodec(index: number, delta: number) {
		const nextIndex = index + delta;
		if (nextIndex < 0 || nextIndex >= value.codecs.length) return;

		const nextCodecs = [...value.codecs];
		const [codec] = nextCodecs.splice(index, 1);
		nextCodecs.splice(nextIndex, 0, codec);
		updateField('codecs', nextCodecs);
	}

	function removeCodec(index: number) {
		updateField(
			'codecs',
			value.codecs.filter((_, currentIndex) => currentIndex !== index)
		);
	}

	function handleAddCodec(event: Event) {
		const select = event.currentTarget as HTMLSelectElement;
		const codec = select.value as VideoCodec;
		if (codec && !value.codecs.includes(codec)) {
			updateField('codecs', [...value.codecs, codec]);
		}
		select.value = '';
	}
</script>

<div class="space-y-5">
	<div class="grid grid-cols-1 gap-4 md:grid-cols-2">
		<div class="space-y-2">
			<Label for="source-video-max-quality">视频最高质量</Label>
			<select
				id="source-video-max-quality"
				value={value.video_max_quality}
				{disabled}
				onchange={(event) =>
					updateField('video_max_quality', (event.currentTarget as HTMLSelectElement).value as VideoQuality)}
				class="border-input bg-background ring-offset-background focus-visible:ring-ring flex h-10 w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
			>
				{#each videoQualityOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</div>

		<div class="space-y-2">
			<Label for="source-video-min-quality">视频最低质量</Label>
			<select
				id="source-video-min-quality"
				value={value.video_min_quality}
				{disabled}
				onchange={(event) =>
					updateField('video_min_quality', (event.currentTarget as HTMLSelectElement).value as VideoQuality)}
				class="border-input bg-background ring-offset-background focus-visible:ring-ring flex h-10 w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
			>
				{#each videoQualityOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</div>

		<div class="space-y-2">
			<Label for="source-audio-max-quality">音频最高质量</Label>
			<select
				id="source-audio-max-quality"
				value={value.audio_max_quality}
				{disabled}
				onchange={(event) =>
					updateField('audio_max_quality', (event.currentTarget as HTMLSelectElement).value as AudioQuality)}
				class="border-input bg-background ring-offset-background focus-visible:ring-ring flex h-10 w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
			>
				{#each audioQualityOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</div>

		<div class="space-y-2">
			<Label for="source-audio-min-quality">音频最低质量</Label>
			<select
				id="source-audio-min-quality"
				value={value.audio_min_quality}
				{disabled}
				onchange={(event) =>
					updateField('audio_min_quality', (event.currentTarget as HTMLSelectElement).value as AudioQuality)}
				class="border-input bg-background ring-offset-background focus-visible:ring-ring flex h-10 w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
			>
				{#each audioQualityOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</div>
	</div>

	<div class="space-y-2">
		<Label>编解码器优先级</Label>
		<p class="text-muted-foreground text-xs">越靠前优先级越高；至少保留一个编码，避免没有可用视频流。</p>
		<div class="space-y-2">
			{#each value.codecs as codec, index (codec)}
				<div class="flex items-center gap-2 rounded-lg border bg-gray-50 p-2 dark:bg-gray-900">
					<span
						class="bg-primary text-primary-foreground flex h-6 w-6 items-center justify-center rounded-full text-xs font-medium"
					>
						{index + 1}
					</span>
					<span class="flex-1 text-sm font-medium">
						{codecOptions.find((option) => option.value === codec)?.label ?? codec}
					</span>
					<Button
						type="button"
						size="sm"
						variant="outline"
						disabled={disabled || index === 0}
						onclick={() => moveCodec(index, -1)}
					>
						上移
					</Button>
					<Button
						type="button"
						size="sm"
						variant="outline"
						disabled={disabled || index === value.codecs.length - 1}
						onclick={() => moveCodec(index, 1)}
					>
						下移
					</Button>
					<Button
						type="button"
						size="sm"
						variant="ghost"
						disabled={disabled || value.codecs.length <= 1}
						onclick={() => removeCodec(index)}
					>
						移除
					</Button>
				</div>
			{/each}

			{#if value.codecs.length < codecOptions.length}
				<select
					value=""
					{disabled}
					onchange={handleAddCodec}
					class="border-input bg-background h-9 w-full rounded-md border px-3 py-2 text-sm disabled:cursor-not-allowed disabled:opacity-50"
				>
					<option value="" disabled>添加编解码器...</option>
					{#each codecOptions as option (option.value)}
						{#if !value.codecs.includes(option.value)}
							<option value={option.value}>{option.label}</option>
						{/if}
					{/each}
				</select>
			{/if}
		</div>
	</div>

	<div class="grid grid-cols-1 gap-3 md:grid-cols-2">
		<label class="flex items-center gap-2 text-sm">
			<input
				type="checkbox"
				checked={value.no_dolby_video}
				{disabled}
				onchange={(event) => updateBoolean('no_dolby_video', (event.currentTarget as HTMLInputElement).checked)}
				class="text-primary focus:ring-primary h-4 w-4 rounded border-gray-300"
			/>
			禁用杜比视界
		</label>
		<label class="flex items-center gap-2 text-sm">
			<input
				type="checkbox"
				checked={value.no_dolby_audio}
				{disabled}
				onchange={(event) => updateBoolean('no_dolby_audio', (event.currentTarget as HTMLInputElement).checked)}
				class="text-primary focus:ring-primary h-4 w-4 rounded border-gray-300"
			/>
			禁用杜比全景声
		</label>
		<label class="flex items-center gap-2 text-sm">
			<input
				type="checkbox"
				checked={value.no_hdr}
				{disabled}
				onchange={(event) => updateBoolean('no_hdr', (event.currentTarget as HTMLInputElement).checked)}
				class="text-primary focus:ring-primary h-4 w-4 rounded border-gray-300"
			/>
			禁用HDR
		</label>
		<label class="flex items-center gap-2 text-sm">
			<input
				type="checkbox"
				checked={value.no_hires}
				{disabled}
				onchange={(event) => updateBoolean('no_hires', (event.currentTarget as HTMLInputElement).checked)}
				class="text-primary focus:ring-primary h-4 w-4 rounded border-gray-300"
			/>
			禁用Hi-Res音频
		</label>
	</div>
</div>
