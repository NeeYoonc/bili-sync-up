# 前端可复用/可合并项检查记录

检查时间：2026-04-16
范围：`web/src/routes`、`web/src/lib/components`、`web/src/lib/utils`

## 本次已落地的复用改造

### 1. 认证型 SSE/live 地址构建逻辑已收敛

原本以下页面都各自做了一遍：

- 从 `localStorage` 读取 `auth_token`
- 拼接 `URLSearchParams`
- 生成 `/api/*/live?...&token=...`

涉及页面：

- `web/src/routes/videos/+page.svelte`
- `web/src/routes/video-sources/+page.svelte`
- `web/src/routes/queue/+page.svelte`

本次新增：

- `web/src/lib/utils/live-stream.ts`
  - `buildAuthenticatedStreamUrl(path, params)`

收益：

- 避免 3 处重复实现
- 统一 token 缺失时的返回行为
- 后续如果 live 接口追加公共参数，只需要改一个地方

### 2. 时间格式化失败兜底逻辑已收敛

原本多处都重复做：

- `formatTimestamp(...)`
- 判断是否为 `无效时间` / `格式化失败`
- 回退到原始值或默认值

本次已收敛到：

- `web/src/lib/utils/timezone.ts`
  - `isInvalidFormattedTime(value)`
  - `formatTimestampOrFallback(timestamp, timezone, format, fallback)`

已替换位置：

- `web/src/routes/+layout.svelte`
- `web/src/routes/+page.svelte`
- `web/src/routes/add-source/+page.svelte`
- `web/src/routes/queue/+page.svelte`
- `web/src/routes/video-sources/+page.svelte`
- `web/src/lib/components/submission-selection-dialog.svelte`

收益：

- 去掉重复的无效时间判断
- 保持页面展示行为一致
- 后续时间格式化策略调整时只改一个公共入口

### 3. SSE 生命周期管理做了第二轮收敛

原本以下页面都各自维护：

- `EventSource` 实例
- 当前 stream url
- `stop()` 关闭逻辑
- `start()` 中的“同 URL 不重复连接”逻辑
- `onerror` 仅对当前连接生效的保护

涉及页面：

- `web/src/routes/videos/+page.svelte`
- `web/src/routes/video-sources/+page.svelte`
- `web/src/routes/queue/+page.svelte`

本次新增：

- `web/src/lib/utils/live-event-source.ts`
  - `createManagedEventSource()`

本次改造后：

- 页面只保留自己的业务事件处理
- 通用连接管理下沉到公共 helper
- `videos` 页面仍保留自己的 `liveUpdateStatus` 业务状态控制

收益：

- 去掉重复的 EventSource 管理样板代码
- 降低三页后续各自改坏连接逻辑的风险
- 为后续更多 live 页面接入提供统一模式

### 4. 投稿列表展示 helper 与选择工具栏完成第三轮收敛

原本投稿列表相关 UI 在两个位置各自维护：

- 日期展示格式化
- 播放/弹幕等数值展示格式化
- 搜索输入框 + 搜索中 spinner
- 全选 / 全不选 / 反选按钮组
- 已选择数量统计

涉及位置：

- `web/src/routes/add-source/+page.svelte`
- `web/src/lib/components/submission-selection-dialog.svelte`

本次新增：

- `web/src/lib/utils/submission.ts`
  - `formatSubmissionDateLabel(pubtime)`
  - `formatSubmissionMetricLabel(count)`
- `web/src/lib/components/submission-selection-toolbar.svelte`

本次改造后：

- 投稿日期与数值展示统一由公共 helper 负责
- 投稿选择区搜索与批量选择操作统一到公共组件
- `add-source` 与投稿选择弹窗共享同一套工具栏实现

收益：

- 去掉重复的投稿日期/数值格式化函数
- 去掉重复的“搜索 + 批量选择”工具栏样板代码
- 后续如果修改投稿选择交互，只需要改一个组件

### 5. 页面级标题/操作区已统一到 SectionHeader

本次已将以下页面头部切换到统一组件：

- `web/src/routes/videos/+page.svelte`
- `web/src/routes/video-sources/+page.svelte`
- `web/src/routes/queue/+page.svelte`
- `web/src/routes/add-source/+page.svelte`

收益：

- 页面标题、描述和操作按钮布局风格统一
- 后续新增页面头部时可直接复用 `SectionHeader`
- 减少页面级重复布局代码

## 当前检查范围内的复用项已完成

本轮记录中的剩余项已经全部落地处理：

- 投稿日期展示 helper：已完成
- 页面级标题/操作区统一：已完成
- 重复搜索区/批量选择工具栏统一：已完成

## 本次修改文件

- `web/src/lib/utils/timezone.ts`
- `web/src/lib/utils/live-stream.ts`
- `web/src/lib/utils/live-event-source.ts`
- `web/src/lib/utils/submission.ts`
- `web/src/lib/components/submission-selection-toolbar.svelte`
- `web/src/routes/+layout.svelte`
- `web/src/routes/+page.svelte`
- `web/src/routes/add-source/+page.svelte`
- `web/src/routes/queue/+page.svelte`
- `web/src/routes/video-sources/+page.svelte`
- `web/src/routes/videos/+page.svelte`
- `web/src/lib/components/submission-selection-dialog.svelte`

## 结论

本次按三轮完成了前端可复用项收敛：

1. 认证型 SSE/live URL 构建统一
2. 时间格式化失败兜底统一
3. SSE 生命周期管理统一
4. 投稿列表日期/数值展示 helper 统一
5. 投稿选择搜索与批量操作工具栏统一
6. 页面级标题/操作区统一到 `SectionHeader`

当前检查范围内识别出的前端复用点已经全部落地完成，并已通过 Node 20 的 `check` / `build` 验证。
