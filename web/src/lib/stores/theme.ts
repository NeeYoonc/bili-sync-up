import { writable } from 'svelte/store';
import { browser } from '$app/environment';

export type Theme = 'light' | 'dark' | 'system';

// 主题存储
export const theme = writable<Theme>('system');

// 当前实际应用的主题模式
export const isDark = writable<boolean>(false);

// 获取系统偏好的主题
function getSystemTheme(): boolean {
	if (!browser) return false;
	return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

// 从localStorage加载主题设置
function loadTheme(): Theme {
	if (!browser) return 'system';
	const stored = localStorage.getItem('bili-sync-theme');
	if (stored && ['light', 'dark', 'system'].includes(stored)) {
		return stored as Theme;
	}
	return 'system';
}

// 保存主题设置到localStorage
function saveTheme(newTheme: Theme) {
	if (!browser) return;
	localStorage.setItem('bili-sync-theme', newTheme);
}

// 应用主题到DOM
function applyTheme(currentTheme: Theme) {
	if (!browser) return;

	const root = document.documentElement;
	const isSystemDark = getSystemTheme();

	let shouldBeDark = false;

	switch (currentTheme) {
		case 'dark':
			shouldBeDark = true;
			break;
		case 'light':
			shouldBeDark = false;
			break;
		case 'system':
			shouldBeDark = isSystemDark;
			break;
	}

	if (shouldBeDark) {
		root.classList.add('dark');
	} else {
		root.classList.remove('dark');
	}
	root.style.colorScheme = shouldBeDark ? 'dark' : 'light';

	isDark.set(shouldBeDark);
}

// 设置主题
export function setTheme(newTheme: Theme) {
	theme.set(newTheme);
	saveTheme(newTheme);
	applyTheme(newTheme);
}

// 切换主题
export function toggleTheme() {
	theme.update((current) => {
		const newTheme = current === 'dark' ? 'light' : 'dark';
		saveTheme(newTheme);
		applyTheme(newTheme);
		return newTheme;
	});
}

// 初始化主题
export function initTheme() {
	if (!browser) return;

	const savedTheme = loadTheme();
	theme.set(savedTheme);
	applyTheme(savedTheme);

	// 监听系统主题变化
	const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
	mediaQuery.addEventListener('change', () => {
		theme.update((current) => {
			if (current === 'system') {
				applyTheme('system');
			}
			return current;
		});
	});
}

// 订阅主题变化并应用
if (browser) {
	theme.subscribe(applyTheme);
}
