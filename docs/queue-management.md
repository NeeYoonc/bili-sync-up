---
title: 任务队列管理
---

# 任务队列管理

> 本文按当前版本的实际实现整理。页面上的按钮位置和文字可能随版本调整，以实际界面为准。

bili-sync 会把「添加视频源」「删除视频源」「删除视频」「刷新弹幕」「保存配置」「AI 批量重命名」这类需要读写数据库、又可能和扫描/下载同时进行的操作放进后台队列，按顺序执行。这样扫描过程中删除视频源不会卡住数据库，也不会留下删一半的状态。

## 在哪里看

Web 界面左侧导航 → **任务队列**（`/queue`）。页面分三块：

1. **队列状态卡片**：视频源删除队列、视频删除队列、添加队列、配置队列、弹幕刷新队列、AI 重命名队列，每张卡片显示队列长度，正在执行时显示「处理中」。
2. **队列详情**：列出每个队列里等待中的任务（任务描述、创建时间）。等待中的任务可以点「取消」；已经开始执行的任务不能取消。
3. **YouTube 下载队列**：单独展示，显示等待 / 下载中 / 跳过 / 失败，失败的任务可以点「重试」。

页面通过 SSE 实时刷新，不需要手动刷新浏览器。

## 六类队列

| 队列 | 什么时候产生 | 说明 |
| --- | --- | --- |
| 视频源删除队列 | 在「视频源」页删除一个视频源 | 删除源记录及其下的视频，是否同时删除本地文件取决于删除时的选择 |
| 视频删除队列 | 删除单个视频 | 只处理这一条视频 |
| 添加队列 | 添加视频源时 | 拉取源信息并建立记录 |
| 配置队列 | 保存设置等配置变更 | 分「更新」和「重载」两组；扫描进行中保存设置会先排队，扫描结束后自动处理 |
| 弹幕刷新队列 | 触发刷新弹幕 | 重新拉取并生成弹幕文件 |
| AI 重命名队列 | 在视频源页面触发「AI 批量重命名」 | 任务写进 `task_queue` 表，可持久化、重启恢复；同一视频源已在排队时不会重复入队，细节见下一节 |

YouTube 源的下载任务单独排队展示（见上面第三块），不属于这六类队列。

## AI 批量重命名队列

「AI 批量重命名」是对整个视频源里已下载的视频批量改写文件名。这个操作以前是在 HTTP 请求里同步跑完的：重命名 100 个文件可能要 1~30 分钟，而客户端或反向代理通常 30 秒就断开连接——连接一断，正在等待的请求被丢弃，任务直接丢失，页面上也看不到任何进度。所以现在改成「入队 + 后台执行」：

- `POST /api/{source_type}/{id}/ai-rename-history` 只做参数校验，然后把任务写进 `task_queue`（`task_type = ai_rename_history`）和内存队列，**立即返回 `queued: true`**。返回时 `renamed_count` 固定是 0，表示「只是入队」，不是「一个文件都没改」。
- 同一视频源已经有等待中的任务时，重复请求不会重复入队，返回的 message 会提示「已在队列中」。
- 真正的重命名由后台的 `process_ai_rename_history_tasks` 串行执行，一次只跑一个任务；执行期间会暂停定时扫描，结束后自动恢复，避免扫描和批量改名同时动文件。
- `source_type` 支持 `collection` / `favorite` / `submission` / `watch_later` / `bangumi` / `pugv` / `manga` / `youtube` / `douyin` / `tiktok`；请求里没传提示词时，回退到视频源自己的配置。AI 重命名的总开关没开时任务会直接失败，队列状态里能看到失败记录。

**查进度**：队列页的「AI 重命名队列」卡片显示排队数量和处理中状态；也可以直接看 `GET /api/queue-status` 返回的 `ai_rename_queue` 字段。这里只有队列长度和 `is_processing`，没有按文件计的百分比进度——任务执行完会在日志里打一行「重命名 X 个，跳过 Y 个，失败 Z 个」，同时视频列表会自动刷新。

**取消**：用 `DELETE /api/queue/tasks/{task_id}` 取消。和其他队列一样，只能取消还在内存队列里等待的任务；**已经开始执行的 AI 批量重命名不能取消**（没有「停止正在执行」这个能力），要么等它跑完，要么等它失败。

**重启恢复**：等待中的任务存在 `task_queue` 表里，进程重启后 `recover_pending_tasks` 会把它们重新推回内存队列继续执行；执行失败的任务标记为 failed 并累加重试次数，不会自动重试。

## 扫描控制：停止 / 恢复 / 任务刷新

首页顶部有任务控制按钮：

- **停止任务**：停止定时扫描，并取消正在进行的下载任务。
- **恢复任务**：恢复扫描和下载，并立即触发一轮扫描。
- **任务刷新**：立即触发一轮扫描，不用等下一次定时扫描；同时会优先处理暂存的配置任务，所以刚保存的设置点一下「任务刷新」可以更快生效。

队列和扫描的关系：扫描进行中时，删除、添加、保存设置这类操作会先排队，等当前操作结束后按顺序执行；进度在「任务队列」页随时可见。

## 队列状态存在哪

任务状态会写入数据库的 `task_queue` 表（pending / processing / completed / failed），所以**程序重启后会自动把待处理的任务恢复到内存队列继续执行**，不会丢。

失败的任务会标记为失败并累加重试次数，不会自动无限重试：重新操作一次即可；如果反复失败，请查看日志（磁盘空间、文件权限、网络都可能导致失败）。

## 用 API 查看和控制

默认服务地址是 `http://<主机>:12345`，端口可以在设置页「服务器端口」修改，改完需要重启。普通接口返回 `{ "status_code": 200, "data": ... }`。

| 方法 | 路径 | 作用 |
| --- | --- | --- |
| GET | `/api/queue-status` | 一次获取六类队列状态（含 `ai_rename_queue`） |
| GET | `/api/queue/live` | SSE 实时推送队列状态（页面用的就是它） |
| DELETE | `/api/queue/tasks/{task_id}` | 取消一条等待中的任务（含 AI 重命名任务）；已开始执行会返回 400 |
| POST | `/api/{source_type}/{id}/ai-rename-history` | 把 AI 批量重命名加入队列，立即返回 `queued: true`，不等待重命名完成 |
| GET | `/api/task-control/status` | 扫描控制状态 |
| POST | `/api/task-control/pause` | 停止扫描和下载 |
| POST | `/api/task-control/resume` | 恢复扫描和下载 |
| POST | `/api/task-control/refresh` | 任务刷新 |
| GET | `/api/youtube/queue-status` | YouTube 下载队列 |
| GET | `/api/douyin/queue-status` | 抖音下载队列 |
| GET | `/api/downloads/progress` | 当前下载进度 |

`GET /api/queue-status` 的 `data` 结构：

```json
{
  "is_scanning": false,
  "delete_queue": { "length": 0, "is_processing": false, "tasks": [] },
  "video_delete_queue": { "length": 0, "is_processing": false, "tasks": [] },
  "add_queue": { "length": 0, "is_processing": false, "tasks": [] },
  "danmaku_queue": { "length": 0, "is_processing": false, "tasks": [] },
  "ai_rename_queue": { "length": 0, "is_processing": false, "tasks": [] },
  "config_queue": {
    "update_length": 0,
    "reload_length": 0,
    "is_processing": false,
    "update_tasks": [],
    "reload_tasks": []
  }
}
```

每个任务对象是 `{ "task_id": "...", "task_type": "...", "description": "...", "created_at": "..." }`，`task_type` 取值为 `delete_video_source`、`delete_video`、`add_video_source`、`refresh_danmaku`、`update_config`、`reload_config`、`ai_rename_history`。AI 重命名任务的 description 形如「AI 批量重命名 collection:12」。

常用操作：

```bash
# 查看队列状态
curl http://localhost:12345/api/queue-status

# 停止 / 恢复 / 立即刷新
curl -X POST http://localhost:12345/api/task-control/pause
curl -X POST http://localhost:12345/api/task-control/resume
curl -X POST http://localhost:12345/api/task-control/refresh

# 取消一条等待中的任务
curl -X DELETE http://localhost:12345/api/queue/tasks/<task_id>

# 把某个视频源的 AI 批量重命名加入队列（source_type 按源类型替换）
curl -X POST http://localhost:12345/api/collection/12/ai-rename-history
```

## 常见问题

**Q：队列一直不动？**

先看首页任务控制状态是不是「任务已暂停」或「正在扫描中」，以及队列卡片有没有「处理中」。暂停了就点「恢复任务」；长时间没有进展就看日志里有没有报错。

**Q：删除任务失败了怎么办？**

队列页会显示失败状态。重新执行一次操作即可；反复失败请检查磁盘空间、文件权限和网络，常见原因见「常见问题」页面。

**Q：点了「AI 批量重命名」，接口很快就返回了，是不是没执行？**

不是。返回 `queued: true` 只表示任务已经入队，真正的重命名在后台跑。去「任务队列」页看「AI 重命名队列」卡片，或调 `/api/queue-status` 看 `ai_rename_queue.is_processing`；执行完日志里会有「重命名 X 个，跳过 Y 个，失败 Z 个」，视频列表也会自动刷新。

**Q：能取消正在执行的任务吗？**

不能。队列页只能取消还在等待的任务；正在下载的任务可以在首页点「停止任务」取消；AI 批量重命名一旦开始执行，只能等它跑完或失败。

**Q：程序重启后队列会丢吗？**

不会。待处理任务存在数据库里，重启后会自动恢复并继续执行。

## 排错

- 接口报错会以通知形式显示在界面右上角。
- 服务日志里可以搜 `队列`、`任务队列`、`task-control` 等关键字，日志位置见「使用教程」。
- AI 批量重命名失败时，先确认设置里「AI 重命名」总开关和模型配置可用，再到日志里搜 `AI 批量重命名任务执行失败` 看具体原因。
- 也可以直接用浏览器打开 `http://<主机>:12345/api/queue-status` 看原始 JSON。
