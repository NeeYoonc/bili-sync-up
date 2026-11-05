# bili-sync

![bili-sync](https://socialify.git.ci/NeeYoonc/bili-sync-up/image?custom_description=%E7%94%B1%20Rust%20%26%20Tokio%20%E9%A9%B1%E5%8A%A8%E7%9A%84%E5%93%94%E5%93%A9%E5%93%94%E5%93%A9%E5%90%8C%E6%AD%A5%E5%B7%A5%E5%85%B7%EF%BC%88%E5%8D%87%E7%BA%A7%E4%BC%98%E5%8C%96%E7%89%88%EF%BC%89&description=1&font=KoHo&issues=1&language=1&logo=https%3A%2F%2Fs2.loli.net%2F2023%2F12%2F02%2F9EwT2yInOu1d3zm.png&name=1&owner=1&pattern=Signal&pulls=1&stargazers=1&theme=Light)

[![GitHub Release](https://img.shields.io/github/v/release/NeeYoonc/bili-sync-up)](https://github.com/NeeYoonc/bili-sync-up/releases/latest)
[![Build](https://github.com/NeeYoonc/bili-sync-up/actions/workflows/build.yml/badge.svg)](https://github.com/NeeYoonc/bili-sync-up/actions/workflows/build.yml)
[![Docker Build](https://github.com/NeeYoonc/bili-sync-up/actions/workflows/docker-build.yml/badge.svg)](https://github.com/NeeYoonc/bili-sync-up/actions/workflows/docker-build.yml)
[![Downloads](https://img.shields.io/github/downloads/NeeYoonc/bili-sync-up/total)](https://github.com/NeeYoonc/bili-sync-up/releases)

专为 NAS 用户打造的哔哩哔哩同步工具，基于 Rust & Tokio 构建。

📚 [在线文档](https://NeeYoonc.github.io/bili-sync-up/) | 🚀 [快速开始](#快速开始) | 📝 [更新日志](./docs/changelog.md) | 💬 [QQ群交流](https://qm.qq.com/q/144RN3oaLM) | 📦 [群晖套件源](https://ssp.dnof.net/)

## ✨ 核心特性

### 🚀可直接原项目迁移运行
- **迁移后只需调整个别功能设置**

### 🎯 智能化功能
- **充电视频智能识别** - 自动检测并处理充电专享视频，无需人工干预
- **失败任务智能筛选** - 一键筛选失败任务，快速定位问题
- **任务队列持久化** - 程序重启后自动恢复任务状态
- **配置热重载** - 修改配置立即生效，无需重启

### 🎬 视频源支持
- **收藏夹** - 直接显示用户所有收藏夹，支持快速选择
- **UP主投稿** - 输入UP主ID查看所有合集/系列
- **稍后再看** - 自动同步稍后再看列表
- **番剧下载** - 支持单季和全季下载模式

### 🚀 技术优势
- **高性能** - Rust + Tokio 异步架构，支持高并发
- **内存优化** - 智能内存数据库模式，提升扫描性能
- **Web管理** - 友好的 Web 界面，无需命令行操作
- **自动重试** - 智能错误处理和重试机制

## 🚀 快速开始

### 群晖 NAS 套件安装（推荐）

**方式一：添加套件源（可在套件中心直接安装 / 升级）**

1. DSM → **套件中心** → **设置** → **套件来源** → **新增**
2. 名称随意（如 `dnof`），位置填：`https://ssp.dnof.net/`
3. 保存后回到「套件中心 → **社群**」，找到 **Bili Sync Up** → 安装

**方式二：手动安装 `.spk`**

1. 到 [Releases](https://github.com/NeeYoonc/bili-sync-up/releases/latest) 下载对应架构：
   - x86_64 机型：`bili-sync-up_x86_64-<版本>.spk`
   - ARM 机型：`bili-sync-up_armv8-<版本>.spk`
2. 套件中心 → **手动安装** → 选择下载的 `.spk`

安装向导可设置 **Web 访问端口**（默认 12345）、**访问 Token**、是否放行防火墙。
装完点套件中心「打开」，或访问 `http://<NAS-IP>:<端口>`（填了 Token 会先要求输入）。

> 套件以专用用户 `bili-sync-up` 运行（DSM 7 套件规范）。下载目录建议用安装时自动创建的共享文件夹 `bili-sync-up`；
> 若要指向其它共享文件夹，需在「控制面板 → 共享文件夹 → 权限」中给 `bili-sync-up` 用户读写权限。

### 使用 Docker（推荐）

```bash
docker run -d \
  --name bili-sync \
  -p 12345:12345 \
  -v ./data:/data \
  qq1582185982/bili-sync:latest
```

### docker-compose
```yaml
services:

  bili-sync:
    image: docker.cnb.cool/sviplk.com/docker/bili-sync:latest
    # build:
    #   context: .
    #   dockerfile: Dockerfile
    restart: unless-stopped
    network_mode: bridge
    # 该选项请仅在日志终端支持彩色输出时启用，否则日志中可能会出现乱码
    tty: false
    # 非必需设置项，推荐设置为宿主机用户的 uid 及 gid (`$uid:$gid`)
    # 可以执行 `id ${user}` 获取 `user` 用户的 uid 及 gid
    # 程序下载的所有文件权限将与此处的用户保持一致，不设置默认为 Root
    # user: 1000:1000
    hostname: bili-sync
    container_name: bili-sync
    # 程序默认绑定 0.0.0.0:12345 运行 http 服务
    ports:
      - 12345:12345
    volumes:
      - /volume1/Cloudreve/OD/20/config:/app/.config/bili-sync
      - /volume1/Cloudreve/OD/20:/Downloads #下载目录 在前端直接/Downloads就是下载到/volume1/Cloudreve/OD/20

    environment:
      - TZ=Asia/Shanghai
      - RUST_LOG=None,bili_sync=info
      # 可选：设置执行周期，默认为每天凌晨3点执行
      # - BILI_SYNC_SCHEDULE=0 3 * * *
    # 资源限制（可选）
    # deploy:
    #   resources:
    #     limits:
    #       cpus: '2'
    #       memory: 2G
    #     reservations:
    #       cpus: '0.5'
    #       memory: 500M
```

### 二进制文件

从 [Releases](https://github.com/NeeYoonc/bili-sync-up/releases) 下载对应平台的可执行文件。

### 开发环境

```bash
# 克隆项目
git clone https://github.com/NeeYoonc/bili-sync-up
cd bili-sync-up

# 安装依赖并启动
./make.bat setup
./make.bat dev
```

访问 `http://localhost:12345` 进入管理界面。

## 📸 界面预览

<details>
<summary>点击展开截图</summary>

### 主页界面
![概览](./docs/assets/主页.jpg)

</details>

## 🛠️ 配置说明

首次启动会自动进入设置向导，引导您完成：
- Cookie 配置
- 下载路径设置
- 视频源添加

所有配置支持在 Web 界面实时修改。

## 📂 项目结构

```
├── crates/                 # Rust 后端
│   ├── bili_sync/          # 主应用
│   ├── bili_sync_entity/   # 数据库实体
│   └── bili_sync_migration/# 数据库迁移
├── web/                    # Svelte 前端
├── docs/                   # VitePress 文档
└── scripts/                # 辅助脚本
```

## 🤝 贡献指南

欢迎提交 Issue 和 Pull Request！

1. Fork 本项目
2. 创建功能分支 (`git checkout -b feature/amazing`)
3. 提交更改 (`git commit -m 'Add amazing feature'`)
4. 推送分支 (`git push origin feature/amazing`)
5. 创建 Pull Request

## 📝 许可证

本项目采用 MIT 许可证。

## 🙏 致谢

- [bilibili-API-collect](https://github.com/SocialSisterYi/bilibili-API-collect) - B站接口文档
- [bilibili-api](https://github.com/Nemo2011/bilibili-api) - Python 接口实现参考
- [danmu2ass](https://github.com/gwy15/danmu2ass) - 弹幕下载功能
