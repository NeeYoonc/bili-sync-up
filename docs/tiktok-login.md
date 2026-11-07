# TikTok 登录状态

TikTok 作者主页的公开内容通常无需登录；“我的喜欢”“收藏夹/播放列表”和受限
内容需要有效的登录 Cookie。当前版本由服务端直接请求 TikTok，不会启动本机
Chrome，也不需要远程 Chromium、CDP、socat 或额外容器。

## 推荐：电脑端登录助手

1. 打开 **设置 → TikTok 登录状态**。
2. 下载并解压 `youtube-login-helper.zip`。
3. Chrome 打开 `chrome://extensions`，Edge 打开 `edge://extensions`。
4. 开启开发者模式，选择“加载已解压的扩展程序”，加载
   `youtube-login-extension` 文件夹。
5. 回到 Bili Sync 设置页，打开扩展并点击“连接当前页面”。
6. 点击“打开 TikTok”，在这个电脑浏览器中正常登录 TikTok。
7. 再次打开扩展，点击“传输 TikTok 登录状态”。
8. 回到设置页刷新，状态显示“已导入”即完成。

助手只读取 `tiktok.com`、`tiktokcdn.com` 的 Cookie，并调用：

```text
POST /api/tiktok/cookies
```

它不会读取或传输账号密码。

## 手动导入

如果不安装助手，也可以：

1. 在电脑浏览器中正常登录 TikTok。
2. 使用 Cookie 导出扩展导出包含 HttpOnly Cookie 的 **Netscape** 格式
   `cookies.txt`。
3. 在 **设置 → TikTok 登录状态** 点击“手动导入 cookies.txt”。

导入后，“我的喜欢”和收藏夹会直接携带该 Cookie 请求 TikTok。若服务端无法
验证账号 secUid，可在设置页按提示从浏览器控制台取得 secUid 后手动保存。

## Docker

Docker 部署只需保留原有配置目录卷映射，不需要额外端口、浏览器卷或第二个
容器。登录凭证保存在数据库中，重启后不会丢失；给签名器和 yt-dlp 使用的
临时文件由程序自动维护。

## 返回空列表或登录异常

TikTok 会按出口 IP 风控。出现空响应时，优先让浏览器与 Bili Sync 使用相同的
干净出口节点，或在设置页配置外源代理后重试。当前版本不提供远程 Chromium
浏览器会话模拟。
