# YouTube 登录

YouTube 下载、订阅动态、喜欢的视频和稍后再看使用经过 `yt-dlp` 验证的网页
Cookie。当前 YouTube OAuth 设备码不能为 `yt-dlp` 提供完整鉴权，因此 Docker
部署使用独立 Chromium 登录容器。

## 独立登录容器

主 `bili-sync` 镜像不包含 Chromium、Xvfb、VNC 或 noVNC。登录浏览器来自独立的
多架构镜像，只在使用登录 Compose 文件时下载和运行。

```bash
docker compose \
  -f docker-compose.yml \
  -f docker-compose.youtube-login.yml \
  up -d
```

进入设置页的 **YouTube 登录**：

1. 等待状态显示“独立登录容器已连接”。
2. 点击“打开 Docker 登录浏览器”。
3. 在 Chromium 网页桌面中登录 YouTube，并确认 YouTube 首页右上角显示账号头像。
4. 回到设置页点击“完成登录”。
5. 主程序通过仅容器内部可访问的 CDP 接口读取 `youtube.com` Cookie，用当前
   `yt-dlp` 验证后保存到配置卷。

登录完成后可以停止浏览器容器：

```bash
docker compose \
  -f docker-compose.yml \
  -f docker-compose.youtube-login.yml \
  stop youtube-login
```

已保存到 `./config` 的登录状态不会被删除，主程序会继续使用它下载。

## NAS 或远程访问

登录桌面默认只绑定到 Docker 宿主机的 `127.0.0.1:3001`。需要从局域网访问时，
在 Compose 文件旁创建 `.env`：

```dotenv
YOUTUBE_LOGIN_BIND=0.0.0.0
YOUTUBE_LOGIN_PORT=3001
YOUTUBE_LOGIN_USER=your-user
YOUTUBE_LOGIN_PASSWORD=change-to-a-strong-password
# 使用反向代理或非默认端口时填写浏览器实际可访问的完整地址
# YOUTUBE_LOGIN_PUBLIC_URL=https://nas.example.com:3001
```

登录桌面包含完整浏览器能力。对局域网开放时必须修改默认用户名和密码；不要将其
直接暴露到公网。Chromium 使用自签名 HTTPS 时，首次打开需要在浏览器中确认继续访问。

## 备用方式

未启动独立登录容器时，仍可以在设置页上传 Netscape 格式的 `cookies.txt`。
