import HeartIcon from '@lucide/svelte/icons/heart';
import FolderIcon from '@lucide/svelte/icons/folder';
import UserIcon from '@lucide/svelte/icons/user';
import ClockIcon from '@lucide/svelte/icons/clock';
import TvIcon from '@lucide/svelte/icons/tv';

export const VIDEO_SOURCES = {
	FAVORITE: { type: 'favorite', title: '收藏夹', icon: HeartIcon },
	COLLECTION: { type: 'collection', title: '合集 / 列表', icon: FolderIcon },
	SUBMISSION: { type: 'submission', title: 'UP主投稿', icon: UserIcon },
	WATCH_LATER: { type: 'watch_later', title: '稍后再看', icon: ClockIcon },
	BANGUMI: { type: 'bangumi', title: '番剧', icon: TvIcon }
} as const;

export type VideoSourceType = (typeof VIDEO_SOURCES)[keyof typeof VIDEO_SOURCES]['type'];
