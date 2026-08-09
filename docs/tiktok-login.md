# TikTok 登录状态与浏览器会话模拟

TikTok 的“我的喜欢/关注列表”依赖真实浏览器会话（webmssdk 的 msToken、
security-sdk 等 localStorage 状态）。仅导入 cookies.txt 会被服务端判定为
非登录环境，因此项目提供两套浏览器会话方案。

## 方案一：本机 Chrome（Windows / macOS / 有 Chrome 的 Linux）

1. 使用浏览器扩展（电脑端登录助手）同步 TikTok Cookie 与页面会话状态
   （localStorage）。
2. 服务端在获取“我的喜欢/关注列表”时自动启动本机 Chrome 完成会话模拟。

需要本机安装 Google Chrome / Chromium。

## 方案二：远程 Chromium（Docker / 群晖等无本机 Chrome 的环境，推荐）

在 Docker 中运行 `linuxserver/chromium` 容器并开启远程调试端口，服务端通过
CDP（Chrome DevTools Protocol）连接该远程浏览器完成会话模拟，不需要在
bili-sync 容器内安装 Chrome。

### 1. 启动远程 Chromium 容器

```yaml
chromium:
  image: lscr.io/linuxserver/chromium:latest
  container_name: chromium
  environment:
    - PUID=1000
    - PGID=1000
    - TZ=Asia/Shanghai
    - CHROME_CLI=--remote-debugging-port=9222 --remote-debugging-address=0.0.0.0 --remote-allow-origins=*
  ports:
    - "9222:9222"
  volumes:
    - ./config/chromium:/config
  shm_size: "1gb"
  restart: unless-stopped
```

或者使用 docker run：

```bash
docker run -d --name chromium --restart unless-stopped   -e CHROME_CLI="--remote-debugging-port=9222 --remote-debugging-address=0.0.0.0 --remote-allow-origins=*"   -p 9222:9222   -v /path/to/chromium-config:/config   --shm-size 1gb   lscr.io/linuxserver/chromium:latest
```

> 说明：`--remote-allow-origins=*` 必须保留，否则服务端 WebSocket 连接会被
> Chromium 以 Origin 校验拒绝。

### 2. 在设置页填写地址

打开 **设置 → TikTok 登录状态 → 远程 Chromium 浏览器模拟**，填写：

```text
http://<群晖或宿主IP>:9222
```

点击“测试远程浏览器连接”确认可达，再点击“保存远程浏览器地址”。

- bili-sync 与 chromium 在同一 Docker 网络时可填容器名，如 `http://chromium:9222`。
- 与 bili-sync 分开部署时可填宿主 IP。
- 留空则回退使用方案一的本机 Chrome。

### 3. 使用

保存后，导入 cookies.txt 并同步浏览器会话（localStorage）即可。获取“我的
喜欢/关注列表”时服务端会连接远程 Chromium 创建临时标签页执行会话模拟，
完成后自动关闭标签页。
