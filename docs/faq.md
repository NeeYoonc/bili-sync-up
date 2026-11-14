# 常见问题

## 安装相关

### Q: Docker 容器无法启动？
A: 检查端口是否被占用，确保挂载路径有读写权限。

### Q: 如何查看程序版本？
A: 启动时日志会显示版本号，或访问 Web 界面的设置页面。

## 凭据相关

### Q: 如何获取 B站凭据？
A: 
1. 登录 bilibili.com
2. F12 打开开发者工具
3. Application > Cookies > bilibili.com
4. 找到 SESSDATA、bili_jct、DedeUserID

### Q: 凭据过期了怎么办？
A: 重新获取并在设置页面更新即可，已下载的内容不受影响。

## 下载相关

### Q: 下载失败怎么办？
A: 
- 检查凭据是否有效
- 查看日志了解具体错误
- 系统会自动重试失败的任务

### Q: 支持什么画质？
A: 自动选择账号权限内的最高画质，大会员支持 4K。

### Q: 下载速度慢？
A: 在设置中增加「下载线程数」（默认 4）。这个值对 B 站、抖音等外源生效：
- YouTube 用单独的「YouTube 单文件连接数」（默认 16，范围 1~32），它与「下载线程数」互不影响，调大它能直接给单个 YouTube 文件开更多连接；
- 走代理下载 YouTube 时，单条连接通常 1MB/s 左右就饱和，把「YouTube 单文件连接数」调到 16 以上才能跑满带宽；
- 以上设置对已经开始的下载任务不生效，改完对后续新任务生效。

## 功能相关

### Q: 如何过滤不想要的视频？
A: 在视频源设置中配置过滤规则：
- 时间过滤
- 时长过滤
- 关键词过滤

### Q: 支持弹幕下载吗？
A: 支持，目前只输出 ASS 格式弹幕文件（XML 弹幕暂未支持）。

### Q: 同样是英文视频，中文字幕为什么有时是纯中文、有时是中英双语？
A: 字幕文件是平台返回什么就存什么，程序不做翻译或拼接，差异来自命中的那条字幕轨：
- 上传者传了「中英对照」的人工字幕 → 拿到双语；
- 上传者只翻了中文，或命中 YouTube 的自动翻译轨 → 拿到纯中文；
- 视频只有原语言的自动字幕 → 拿到原语言字幕。

想要稳定拿到双语，把视频源的「AI 字幕优先语言」选成「中文 + 英语（双语）」（对应的值写法是 `zh-CN+en`）：
- 程序会分别下载中文字幕轨和英文字幕轨，按**时间轴重叠度**合并成一条 `xxx.zh-CN-en.vtt`，英文在上、中文在下，与两条轨的行数是否一致无关；
- 自动识别（ASR）轨里的 `<c>` / `<00:00:01.000>` 内联标签会被清洗掉；
- 找不到对照语言轨时自动降级为单语字幕，不会让整个字幕任务失败；
- 中文字幕轨本身已经是「中英对照」（常见于人工上传的双语字幕）时，不会把英文再叠一遍，也不会多生成一份内容重复的合并文件；
- 自动翻译轨（`tlang=`）容易触发 429，候选轨之间会自动退避重试并依次回退。

### Q: B 站视频也能拿到中英双语字幕吗？
A: 可以，同样把「AI 字幕优先语言」选成「中文 + 英语（双语）」。B 站的落盘方式和 YouTube 不同：

- 每条字幕轨依旧各自写成 `{视频名}.{语言}.srt`（如 `xxx.zh-CN.srt`、`xxx.en-US.srt`），这是原有行为，未做改动；
- 额外再合并出一条 `{视频名}.zh-CN-en.srt`，时间轴以目标语言轨为准，英文在上、中文在下，播放器里即双语同屏；
- 只有一条轨、或两条轨语言相同（如 `zh-CN` 与 `zh-Hans`）时不生成合并文件，日志会给出已下载语言列表，不会让字幕子任务失败；
- `{视频名}.{语言}.srt` 里的那条字幕本身已是中英对照时不生成合并文件（日志会写明「本身已是双语」），避免播放器里出现两条内容相同的字幕轨；
- 开启「下载 AI 字幕」时，双语设置会额外取回对照语言的 AI 轨（如 `ai-en`），否则上游只能拿到单语。

和 YouTube 一样，双语的前提是源站真的提供了两条不同语言的轨；如果上传者只传了一条中英对照的人工轨，拿到的就是那一条，程序不会自行翻译。

### Q: 可以下载会员专享视频吗？
A: 需要使用大会员账号的凭据。

### Q: 一直无法下载，提示Operation not permitted 怎么办？
A: 
- 项目更新后，和教程有点不一样了，特别是更新为webUI，不用在配置文件里面写映射时。
- 现在不用再把下载目录映射为原来固定的某个目录。
- 例如：/media，或者/download，而是直接把你要下载文件的目录映射出来，不用理会项目内部目录。
- 你可以按照这个方法，试一下。希望能帮到你。

## aria2 / yt-dlp 下载失败（国内网络或镜像失效）

**现象**：启用 aria2 或首次使用 YouTube 时，日志出现「下载 aria2 包失败」「下载 yt-dlp 返回错误状态」或校验失败；或者连接 GitHub 长时间无响应。

**原因**：
- 旧版内置的 aria2 下载地址（`r2d.dnof.net`）已改成 API key 鉴权，直接访问会返回 `401 {"error":"Unauthorized","code":"INVALID_API_KEY"}`；当前版本已改用本仓库 Release `tools-aria2` 的压缩包。
- 自动下载走 GitHub，国内网络可能很慢或连不上。

**方式一：手动下载离线包（国内网络推荐）**

国内盘：<https://wwbat.lanzouw.com/b01i60dxoh> 提取码 `3crm`

包含：
- aria2 1.37.0：Windows x64 / x86、Linux x86_64 / aarch64 / armv7 / arm / i686 / LoongArch64 的 musl 静态构建，以及源码；
- yt-dlp 2026.10.26：Windows、Linux（glibc 与 musl 两种）、macOS、armv7l、Python zipapp；
- `CHECKSUMS.txt` 校验和。

注意：glibc 与 musl 构建不能混用；群晖 NAS 用的是 glibc 系统，请选 glibc 版 yt-dlp。

放置路径（`<配置目录>` 见下）：

| 工具 | 放置路径 |
|---|---|
| aria2 | `<配置目录>/tools/aria2/1.37.0/<target_key>/aria2c`（Windows 为 `aria2c.exe`） |
| yt-dlp | `<配置目录>/tools/yt-dlp/<target_key>/yt-dlp`（Windows 为 `yt-dlp.exe`），或设置环境变量 `BILI_SYNC_YTDLP_PATH` 直接指向可执行文件 |

`<target_key>`：Windows x86_64 用 `windows-x86_64`，Linux x86_64 用 `linux-x86_64`，Linux aarch64 用 `linux-aarch64`。

**方式二：配置镜像目录**

把同名压缩包（aria2）或同名资产 + `SHA2-256SUMS`（yt-dlp）放到自己的 Web 目录 / 对象存储，再设置环境变量：

```bash
# aria2 镜像：目录下放 aria2-x86_64-linux-musl_static.zip 等同名压缩包
BILI_SYNC_ARIA2_MIRROR_BASE=https://your-mirror.example.com/aria2

# yt-dlp 镜像：目录下放 yt-dlp_linux 等同名资产和 SHA2-256SUMS
BILI_SYNC_YTDLP_MIRROR_BASE=https://your-mirror.example.com/yt-dlp
```

- 值末尾的 `/` 可写可不写，程序会去掉；
- aria2 仍会校验 sha256，镜像里必须是同名同内容的压缩包；
- yt-dlp 镜像必须同时提供 `SHA2-256SUMS`，程序按它校验下载结果。

**配置目录在哪**：
- Docker：`/app/.config/bili-sync`（容器内路径），对应你挂载出来的配置卷；
- 群晖套件：`/var/packages/bili-sync-up/var/config`（即套件说明里的 `var/config`）；
- Windows：`%APPDATA%\bili-sync`；
- 其它部署方式见 [安装指南](./installation.md)。

放好后重启程序即可，已下载完成的内容不需要重下。

## 故障排除

### Q: Web 界面打不开？
A: 
- 确认程序正在运行
- 检查防火墙设置
- 尝试 http://127.0.0.1:12345

### Q: 日志在哪里查看？
A: Web 界面的"日志"页面可以实时查看，或查看 data 目录下的日志文件。

### Q: 如何重置所有配置？
A: 删除 data 目录下的 data.sqlite 文件，重启程序。

## 媒体服务器相关

### Q: Jellyfin 中字幕显示为方块怎么办？
A: 这是 Jellyfin 字幕渲染的常见问题，特别是 ASS 格式的弹幕字幕。解决方案：

1. **检查 Jellyfin 的字幕渲染设置**
   - 在 Jellyfin 管理面板中：控制台 → 播放 → 字幕
   - 确保字幕模式设置为"始终烧录"或"自动"
   - 不要选择"仅限图像格式"

2. **检查转码设置**
   - 控制台 → 播放 → 转码
   - 启用"允许字幕提取"
   - 字幕编码器设置为"自动"或"srt"

3. **强制字幕烧录**
   - 播放时点击齿轮图标 → 质量
   - 选择一个需要转码的质量（不是"直接播放"）
   - 这会强制 Jellyfin 将字幕烧录到视频中

4. **安装服务器字体（Linux）**
   ```bash
   # 安装中文字体
   sudo apt-get install fonts-noto-cjk
   # 或
   sudo apt-get install fonts-wqy-microhei fonts-wqy-zenhei
   # 刷新字体缓存
   fc-cache -fv
   ```

5. **客户端解决方案**
   - 使用 Chrome/Edge 浏览器效果更好
   - 尝试使用 Jellyfin 客户端而不是 Web 界面
   - 移动设备使用官方 Jellyfin 应用

注意：如果 Emby 能正常显示同样的字幕文件，说明文件本身没问题，是 Jellyfin 的渲染配置问题。

## 其他问题

更多问题请访问 [GitHub Issues](https://github.com/NeeYoonc/bili-sync-up/issues)
