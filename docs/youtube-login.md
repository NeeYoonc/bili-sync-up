# YouTube 登录状态

YouTube 的订阅动态、喜欢的视频、稍后再看和需要账号权限的下载使用经过
`yt-dlp` 验证的网页 Cookie。`bili-sync` 不再内嵌 Chromium，也不会在 Docker
容器或服务端启动登录浏览器。

推荐在日常使用的电脑浏览器中登录 YouTube，再通过项目提供的电脑端登录助手把
登录状态传输到现有 Web 接口。

## 电脑端登录助手

1. 打开 **设置 → YouTube 登录状态**。
2. 下载并解压 `youtube-login-helper.zip`。
3. Chrome 打开 `chrome://extensions`，Edge 打开 `edge://extensions`。
4. 开启开发者模式，选择“加载已解压的扩展程序”，加载
   `youtube-login-extension` 文件夹。
5. 回到 Bili Sync 设置页，打开扩展并点击“连接当前页面”。
6. 点击“打开 YouTube”，在这个电脑浏览器中正常登录 YouTube。
7. 再次打开扩展，点击“传输登录状态”。
8. 回到设置页刷新，状态显示“已导入”即完成。

助手从当前 Bili Sync 页面读取服务地址和 API Token，只读取
`youtube.com` Cookie，并调用现有的：

```text
POST /api/youtube/cookies
```

它不会读取或传输 Google 密码。首次连接 Bili Sync 地址时，浏览器会要求授权
扩展访问该地址。

## 手动导入

如果不安装助手，也可以：

1. 在电脑浏览器中正常登录 YouTube。
2. 使用 Cookie 导出扩展导出 **Netscape** 格式的 `cookies.txt`。
3. 在 **设置 → YouTube 登录状态** 点击“手动导入 cookies.txt”。

服务端会先使用当前 `yt-dlp` 验证文件。验证成功后才替换旧文件，验证失败会保留
原有登录状态。

Cookie 保存在 Bili Sync 配置目录：

```text
youtube-cookies.txt
```

Docker 部署时只需保持原有 `/app/.config/bili-sync` 配置目录卷映射，不需要
额外端口、浏览器卷或第二个容器。
