# YouTube 登录

YouTube 下载、订阅动态、喜欢的视频和稍后再看使用经过 `yt-dlp` 验证的网页
Cookie。Docker 镜像已经内置 Chromium 登录运行时，`bili-sync` 和登录浏览器位于
**同一个容器**，不需要再启动 sidecar 或第二个 Compose 服务。

## Docker 单容器直接登录

更新代码或镜像后，重新构建并启动原有服务：

```bash
docker compose up -d --build --force-recreate
```

进入设置页的 **YouTube 登录**：

1. 等待状态显示“Docker 直接登录已就绪”。
2. 点击“直接登录 YouTube”。
3. 在 Chromium 页面中登录 YouTube，并确认首页右上角显示账号头像。
4. 回到设置页点击“完成登录”。
5. 主程序通过容器内部 CDP 读取 `youtube.com` Cookie，用当前 `yt-dlp` 验证后
   保存到原有配置卷。

Chromium 登录资料保存在：

```text
./config/youtube-login-browser
```

程序 Cookie 仍保存在原有配置卷：

```text
./config/youtube-cookies.txt
```

## 登录页面

Compose 默认发布：

```text
http://Docker主机地址:3001
```

登录页面默认直接打开。需要额外启用 HTTP Basic Auth 时，在 Compose 文件旁
创建 `.env` 并同时填写账号和密码：

```dotenv
YOUTUBE_LOGIN_BIND=0.0.0.0
YOUTUBE_LOGIN_PORT=3001
# YOUTUBE_LOGIN_USER=your-user
# YOUTUBE_LOGIN_PASSWORD=change-to-a-strong-password
# 使用反向代理或不同外部地址时填写完整访问地址
# YOUTUBE_LOGIN_PUBLIC_URL=https://nas.example.com:3001
```

默认登录入口使用 HTTP，避免自签名证书导致 Chromium 阻止访问。需要 HTTPS 时，
通过反向代理配置有效证书，并将完整地址写入 `YOUTUBE_LOGIN_PUBLIC_URL`。

## 为什么不复刻 Google 密码登录请求

Google 登录不是稳定的用户名/密码 HTTP 接口。网页流程包含动态流程令牌、设备
指纹、风控、验证码、两步验证和 WebAuthn，并会按账号与地区变化。抓取一次网络
请求后重放无法形成可靠登录功能，也无法覆盖验证码和二次验证。

项目因此保留真实 Chromium 登录，但把它与主程序整合进同一个 Docker 容器。
`cookies.txt` 导入只作为备用方式。
