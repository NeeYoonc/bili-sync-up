export default {
  title: 'Bili-Sync',
  description: '由 Rust & Tokio 驱动的哔哩哔哩同步工具（升级优化版）',
  base: '/bili-sync-up/',
  lang: 'zh-CN',
  ignoreDeadLinks: true,

  // docs/README.md 是给 GitHub 目录页看的说明，不参与站点构建；
  // SYSTEM_CONFIG_QUEUE_SUMMARY.md 是内部实现笔记；superpowers/plans 是面向智能体的实现计划。
  srcExclude: ['README.md', 'SYSTEM_CONFIG_QUEUE_SUMMARY.md', 'superpowers/plans/**'],

  themeConfig: {
    logo: '/icon.svg',

    nav: [
      { text: '首页', link: '/' },
      { text: '使用指南', link: '/installation' },
      { text: 'GitHub', link: 'https://github.com/NeeYoonc/bili-sync-up' }
    ],

    sidebar: [
      {
        text: '开始使用',
        items: [
          { text: '项目介绍', link: '/introduction' },
          { text: '安装指南', link: '/installation' },
          { text: '快速开始', link: '/quick-start' },
          { text: '使用教程', link: '/usage' }
        ]
      },
      {
        text: '参考',
        items: [
          { text: '功能列表', link: '/features' },
          { text: '常见问题', link: '/faq' },
          { text: '命令行参数', link: '/args' },
          { text: '更新记录', link: '/changelog' }
        ]
      },
      {
        text: '进阶',
        items: [
          { text: '任务队列管理', link: '/queue-management' },
          { text: 'YouTube 登录', link: '/youtube-login' },
          { text: 'TikTok 登录', link: '/tiktok-login' }
        ]
      },
      {
        text: '设计文档',
        items: [
          { text: '源级充电视频重试设计', link: '/superpowers/specs/2026-06-18-retry-charge-videos-design' },
          { text: '哔哩哔哩漫画源设计', link: '/superpowers/specs/2026-10-03-bilibili-manga-source-design' }
        ]
      }
    ],

    search: {
      provider: 'local'
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/NeeYoonc/bili-sync-up' }
    ],

    footer: {
      copyright: 'Copyright © 2026 Bili-Sync'
    }
  }
}
