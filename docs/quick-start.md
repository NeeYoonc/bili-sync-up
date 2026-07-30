# 快速开始

## 安装方式

### 方式一：Docker（推荐）

```bash
docker run -d \
  --name bili-sync \
  -p 12345:12345 \
  -p 3001:3001 \
  -v /path/to/data:/app/.config/bili-sync \
  -v /path/to/youtube-browser:/config \
  -v /path/to/videos:/app/videos \
  -e CUSTOM_USER=bili-sync \
  -e PASSWORD=change-me \
  qq1582185982/bili-sync:latest
```

### 方式二：下载二进制文件

1. 从 [GitHub Releases](https://github.com/NeeYoonc/bili-sync-up/releases) 下载对应平台的二进制文件
2. 解压并运行：
   ```bash
   ./bili-sync-rs
   ```

### 方式三：从源码编译

```bash
git clone https://github.com/NeeYoonc/bili-sync-up.git
cd bili-sync-up
cargo build --release
./target/release/bili-sync-rs
```

## 使用步骤

### 1. 访问 Web 界面

打开浏览器访问 `http://localhost:12345`

### 2. 配置凭据

在设置页面配置你的 B站凭据：
- **SESSDATA**：B站登录凭据
- **bili_jct**：CSRF token
- **DedeUserID**：用户ID

> 💡 获取方法：登录 B站后，F12 打开开发者工具，在 Application > Cookies 中查找

YouTube 可在设置页点击“直接登录 YouTube”，通过同一 Docker 容器的
`https://主机地址:3001` 完成登录，不需要第二个容器。

### 3. 添加视频源

点击"添加视频源"，选择类型：
- **UP主投稿**：输入 UP主主页链接
- **收藏夹**：输入收藏夹链接
- **视频合集**：输入合集链接
- **番剧**：输入番剧链接

### 4. 开始下载

系统会自动开始扫描和下载。你可以在主页查看：
- 📊 下载进度
- 📝 实时日志
- 🚦 任务队列状态

## 常用配置

### 基础设置
- **扫描间隔**：默认 10 分钟
- **下载线程数**：默认 16
- **视频质量**：默认最高画质

### 过滤规则
- **时间过滤**：只下载指定时间后的视频
- **时长过滤**：过滤过短或过长的视频
- **关键词过滤**：排除特定标题的视频

## 下一步

- 查看 [功能一览](./features.md) 了解全部功能
- 阅读 [使用教程](./usage.md) 进行详细配置
- 遇到问题？查看 [常见问题](./faq.md)
