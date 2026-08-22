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

抖音同步说明：
- “传输抖音登录状态”会同时传输登录 Cookie（cookies.txt）和「我的喜欢/收藏夹」接口
  签名所需的会话密钥（浏览器 localStorage/sessionStorage 与 IndexedDB 中的
  security-sdk/SLARDAR 等，保存为 douyin-secsdk.json）。
- 同步抖音前请保持已登录的抖音网页（www.douyin.com）打开；助手会自动遍历所有抖音
  标签页并合并密钥，因此即使开着多个抖音页面（如 live/creator 子域）也能拿到密钥。
- 导入成功后状态栏会显示“我的喜欢/收藏夹签名会话已同步 ✓”；如果仍提示缺少
  douyin-secsdk.json，说明浏览器加载的还是旧版助手，请删除旧扩展后重新加载 v1.5.2。
- 仅手动导入 cookies.txt 可以用于作者作品扫描；要拉取「我的喜欢」「收藏夹」，
  请使用电脑端登录助手的“传输抖音登录状态”完整同步一次。

TikTok 同步说明：
- “同步 TikTok 登录状态”仅传输登录 Cookie（cookies.txt）。
- TikTok 的“我的喜欢/关注列表”只需 cookies.txt 即可拉取；若返回空响应，
  通常是当前出口 IP 被 TikTok 风控，请更换干净的出口 IP 或配置外源代理。
- 同步前请保持 tiktok.com 页面打开且处于登录状态；若会话过期（登录态失效），刷新页面后重新同步。
