import HeartIcon from '@lucide/svelte/icons/heart';
import FolderIcon from '@lucide/svelte/icons/folder';
import UserIcon from '@lucide/svelte/icons/user';
import ClockIcon from '@lucide/svelte/icons/clock';
import TvIcon from '@lucide/svelte/icons/tv';
import YoutubeIcon from '@lucide/svelte/icons/youtube';

export const VIDEO_SOURCES = {
	FAVORITE: { type: 'favorite', title: '收藏夹', icon: HeartIcon },
	COLLECTION: { type: 'collection', title: '合集 / 列表', icon: FolderIcon },
	SUBMISSION: { type: 'submission', title: 'UP主投稿', icon: UserIcon },
	WATCH_LATER: { type: 'watch_later', title: '稍后再看', icon: ClockIcon },
	BANGUMI: { type: 'bangumi', title: '番剧', icon: TvIcon },
	YOUTUBE: { type: 'youtube', title: 'YouTube 视频源', icon: YoutubeIcon }
} as const;

export const DANMAKU_SYNC_STAGE_LABELS: Record<number, string> = {
	0: '未同步',
	1: '新鲜期',
	2: '成熟期',
	3: '老化期',
	4: '已冻结'
};

export type VideoSourceType = (typeof VIDEO_SOURCES)[keyof typeof VIDEO_SOURCES]['type'];
