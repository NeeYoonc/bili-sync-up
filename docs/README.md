# bili-sync

> 🎬 强大的 Bilibili 视频批量下载和管理工具

## ✨ 主要功能

- 📺 支持 UP主投稿、收藏夹、合集、番剧、稍后观看等多种视频源
- 🔄 自动增量扫描，智能跳过已下载内容
- 🎯 灵活的过滤规则和下载策略
- 📱 现代化 Web 界面，支持实时监控
- 🚀 高性能多线程下载，支持断点续传

## 🚀 快速开始

```bash
# Docker 一键启动
docker run -d -p 12345:12345 -v /path/to/data:/app/.config/bili-sync -v /path/to/videos:/Downloads qq1582185982/bili-sync

# 或下载二进制文件直接运行
./bili-sync-rs
```

访问 `http://localhost:12345` 开始使用

## 📖 文档

- [项目介绍](./introduction.md)
- [快速上手](./quick-start.md)
- [功能一览](./features.md)
- [使用教程](./usage.md)
- [更新记录](./changelog.md)

## 📝 许可证

MIT License