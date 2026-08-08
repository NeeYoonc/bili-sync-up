Bili Sync 外部平台登录助手
============================

Chrome：
1. 解压下载的 youtube-login-helper.zip。
2. 打开 chrome://extensions。
3. 开启“开发者模式”，点击“加载已解压的扩展程序”。
4. 选择解压后的 youtube-login-extension 文件夹。

Edge：
1. 解压下载的 youtube-login-helper.zip。
2. 打开 edge://extensions。
3. 开启“开发人员模式”，点击“加载解压缩的扩展”。
4. 选择解压后的 youtube-login-extension 文件夹。

使用：
1. 打开 Bili Sync 设置 -> YouTube 登录状态、抖音登录状态或 TikTok 登录状态。
2. 点击浏览器工具栏中的“Bili Sync 外部平台登录助手”。
3. 点击“连接当前页面”。
4. 点击“打开 YouTube”、“打开抖音”或“打开 TikTok”，在当前电脑浏览器中正常登录。
5. 再次打开助手，点击对应平台的“传输登录状态”。

助手只读取并传输 youtube.com、google.com 中维持 YouTube 会话的 Cookie、
douyin.com、bytedance.com Cookie，或 tiktok.com、tiktokcdn.com Cookie；
不读取或传输账号密码。

TikTok 同步说明（1.3.0+）：
- “同步 TikTok 会话”会同时传输 Cookie 与页面会话状态（localStorage）。
- TikTok 的“我的喜欢/关注列表”接口依赖浏览器会话状态（webmssdk 的 msToken/security-sdk 等），
  仅导入 cookies.txt 会被判定为未登录。
- 同步前请保持 tiktok.com 页面打开且处于登录状态；若会话过期（登录态失效），刷新页面后重新同步。
