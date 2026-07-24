import taskLists from "markdown-it-task-lists";
import { defineConfig } from "vitepress";

// https://vitepress.dev/reference/site-config
export default defineConfig({
	title: "bili-sync",
	description: "由 Rust & Tokio 驱动的哔哩哔哩同步工具",
	lang: "zh-Hans",
	base: "/bili-sync-01/", // GitHub仓库名
	sitemap: {
		hostname: "https://qq1582185982.github.io/bili-sync-01",
	},
	lastUpdated: true,
	cleanUrls: true,
	metaChunk: true,
	ignoreDeadLinks: true, // 忽略死链接检查
	themeConfig: {
		outline: {
			label: "页面导航",
			level: "deep",
		},
		// https://vitepress.dev/reference/default-theme-config
		nav: [
			{ text: "主页", link: "/" },
			{
				text: "v3.0.9.5 Latest",
				items: [
					{
						text: "程序更新",
						link: "https://github.com/qq1582185982/bili-sync-01/releases",
					},
					{
						text: "更新日志",
						link: "/changelog",
					},
				],
			},
		],
		sidebar: [
			{
				text: "快速开始",
				items: [
					{ text: "介绍", link: "/introduction" },
					{ text: "快速上手", link: "/quick-start" },
					{ text: "功能一览", link: "/features" },
				],
			},
			{
				text: "使用指南",
				items: [
					{ text: "配置说明", link: "/configuration" },
					{ text: "视频源管理", link: "/video-source-management" },
					{ text: "任务队列", link: "/QUEUE_FEATURE_SUMMARY" },
					{ text: "常见问题", link: "/faq" },
				],
			},
			{
				text: "开发者",
				items: [
					{ text: "技术架构", link: "/technical-architecture" },
					{ text: "API 参考", link: "/api-endpoints" },
					{ text: "更新日志", link: "/changelog" },
				],
			},
		],
		socialLinks: [
			{ icon: "github", link: "https://github.com/qq1582185982/bili-sync-01" },
		],
		search: {
			provider: "local",
		},
		notFound: {
			title: "你来到了没有知识的荒原",
			quote: "这里什么都没有",
			linkText: "返回首页",
		},
		docFooter: {
			prev: "上一页",
			next: "下一页",
		},
		lastUpdated: {
			text: "上次更新于",
		},
		returnToTopLabel: "回到顶部",
		sidebarMenuLabel: "菜单",
		darkModeSwitchLabel: "主题",
		lightModeSwitchTitle: "切换到浅色模式",
		darkModeSwitchTitle: "切换到深色模式",
	},
	markdown: {
		config: (md) => {
			md.use(taskLists);
		},
		theme: {
			light: "github-light",
			dark: "github-dark",
		},
	},
	head: [
		["link", { rel: "icon", type: "image/svg+xml", href: "/icon.svg" }],
		["link", { rel: "icon", type: "image/png", href: "/icon.png" }],
	],
});
