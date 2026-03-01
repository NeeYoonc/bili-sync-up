type GlossaryEntry = {
	terms: string[];
	description: string;
};

type GlossaryMatch = {
	raw: string;
	normalized: string;
	description: string;
};

type SemanticResolver = {
	pattern: RegExp;
	resolve: (term: string, match: RegExpMatchArray) => string;
};

const GLOSSARY_ENTRIES: GlossaryEntry[] = [
	{
		terms: ['主页', '首页', '总览', '仪表盘'],
		description: '系统总览页，集中展示扫描状态、任务状态、资源使用率和统计信息。'
	},
	{
		terms: ['日志', '系统日志'],
		description: '查看程序运行日志，支持按等级筛选和导出问题现场。'
	},
	{
		terms: ['视频', '视频管理'],
		description: '视频内容列表页，支持搜索、筛选、排序、批量重置与批量删除。'
	},
	{
		terms: ['视频源', '视频源管理'],
		description: '管理收藏夹/合集/投稿/番剧等订阅源，并配置每个源的下载开关。'
	},
	{
		terms: ['任务队列'],
		description: '查看当前扫描与下载队列状态，了解任务执行进度和等待情况。'
	},
	{
		terms: ['设置', '系统设置'],
		description: '全局配置中心，包含命名、画质、下载、风控、通知、AI重命名等配置。'
	},
	{
		terms: ['更新记录', '文档', '退出'],
		description: '分别用于查看版本变更、项目文档和退出管理页登录态。'
	},
	{
		terms: ['当前版本', '渠道', '正式版', '测试版', '开发版'],
		description: '显示当前运行版本和发布通道，并可查看本地构建时间与仓库推送时间。'
	},
	{
		terms: ['刷新', '自动刷新中'],
		description: '重新拉取当前页面数据；自动刷新中表示系统会周期性自动刷新。'
	},
	{
		terms: ['保存', '保存设置', '保存凭证'],
		description: '提交当前表单配置并写入后端，后续扫描/下载任务按新配置执行。'
	},
	{
		terms: ['取消', '关闭'],
		description: '关闭当前弹窗或取消本次操作，不会保存未提交修改。'
	},
	{
		terms: ['取消任务', '取消中'],
		description: '取消任务队列中“等待处理”的任务；若任务已进入处理中则无法取消。'
	},
	{
		terms: ['添加', '添加视频源'],
		description: '新增一个视频源订阅，后续扫描任务会纳入该源。'
	},
	{
		terms: ['批量操作', '退出批量', '批量启用', '批量禁用'],
		description: '对多个视频源进行一次性启用/禁用操作，减少逐条切换成本。'
	},
	{
		terms: ['全选', '清空', '取消选中'],
		description: '用于批量选择控制：全选当前列表、清空选择或取消已选项。'
	},
	{
		terms: ['批量删除', '删除选中', '确认删除', '删除'],
		description: '删除视频或视频源；删除后不可直接恢复，执行前请确认目标。'
	},
	{
		terms: ['批量重置', '重置', '强制重置', '确认重置'],
		description: '将任务状态重置到待处理，供系统重新拉取详情或重新下载。'
	},
	{
		terms: ['只重置失败的任务', '重置所有任务类型', '或选择特定任务'],
		description: '控制重置范围：仅失败项、全部任务，或按子任务类型精确重置。'
	},
	{
		terms: ['重置视频封面', '重置视频内容', '重置视频信息', '重置视频弹幕', '重置视频字幕'],
		description: '分别重置封面、媒体流、元数据、弹幕和字幕任务状态。'
	},
	{
		terms: ['搜索视频标题'],
		description: '按视频标题关键字过滤列表，快速定位目标视频。'
	},
	{
		terms: ['添加时间 (最新)', '添加时间 (最早)', '发布时间 (最新)', '发布时间 (最早)', '名称 (A-Z)', '名称 (Z-A)'],
		description: '视频列表排序方式，用于控制列表的时间顺序或名称顺序。'
	},
	{
		terms: ['每页', '每页显示', '每行'],
		description: '控制视频列表单页数量与网格列数，影响浏览密度。'
	},
	{
		terms: ['筛选', '按视频源筛选', '按分辨率筛选', '当前筛选', '清除筛选', '清除所有筛选', '清除分辨率筛选', '只显示错误视频', '清除错误视频筛选'],
		description: '按条件过滤视频列表，聚焦指定视频源、分辨率或错误状态。'
	},
	{
		terms: ['全部分辨率', '360p', '480p', '720p', '1080p', '1440p', '2160p'],
		description: '视频分辨率筛选项；按记录的页面分辨率宽高归类显示。'
	},
	{
		terms: ['加载中...', '保存中...', '删除中...', '验证中...', '重置中...'],
		description: '表示对应操作正在执行中，请等待请求完成。'
	},
	{
		terms: ['暂无视频数据', '暂无可选日志文件（请先运行一轮任务）'],
		description: '当前条件下暂无可展示数据；通常需要先执行扫描任务或调整筛选条件。'
	},
	{
		terms: ['收藏夹', '合集', '系列', '合集/列表', 'UP主投稿', '投稿', '稍后再看', '番剧'],
		description: '视频源类型，决定扫描接口、目录结构和后续处理策略。'
	},
	{
		terms: ['已启用', '已禁用'],
		description: '表示该视频源或开关当前是否参与后续扫描/下载流程。'
	},
	{
		terms: ['扫描已删除视频', '扫描删除视频已启用'],
		description: '开启后会扫描并记录已失效视频，方便后续状态管理或补扫。'
	},
	{
		terms: ['仅音频模式', '仅M4A', '仅保留M4A模式'],
		description: '仅下载音频轨；启用仅M4A时会尽量只保留 m4a 音频文件。'
	},
	{
		terms: ['平铺目录'],
		description: '下载文件不再按层级分目录，直接落到视频源根目录。'
	},
	{
		terms: ['动态API', '动态API已启用'],
		description: '投稿源改用动态接口抓取；适合动态内容较多账号，但请求频率更高。'
	},
	{
		terms: ['下载弹幕', '弹幕下载已禁用'],
		description: '控制是否下载弹幕文件（ASS/XML）并写入本地。'
	},
	{
		terms: ['下载字幕', '字幕下载已禁用'],
		description: '控制是否下载字幕文件（如 SRT/JSON）并写入本地。'
	},
	{
		terms: ['重设路径'],
		description: '修改视频源下载目录；后续任务会按新路径写入并迁移可迁移文件。'
	},
	{
		terms: ['关键词过滤'],
		description: '通过白名单/黑名单过滤视频标题，决定哪些视频允许下载。'
	},
	{
		terms: ['选择历史投稿'],
		description: '为投稿源指定历史视频下载范围，支持只选部分稿件。'
	},
	{
		terms: ['AI重命名设置', 'AI批量重命名历史文件'],
		description: '分别用于配置该源 AI 命名规则与对已下载历史文件执行批量重命名。'
	},
	{
		terms: ['每轮扫描数量', '自适应扫描频率', '最大间隔（小时）'],
		description: '投稿源扫描优化参数：分批扫描 + 长期无更新自动降频，降低风控概率。'
	},
	{
		terms: ['文件命名', '文件命名设置', '文件命名模板'],
		description: '配置视频、分页、合集、番剧文件及目录命名规则。'
	},
	{
		terms: ['视频文件名模板', '单P视频文件名模板', '多P视频文件名模板'],
		description: '控制不同视频形态的文件名格式，支持变量渲染。'
	},
	{
		terms: ['文件夹结构模板'],
		description: '定义目录层级结构，支持在模板中使用路径分隔符创建子目录。'
	},
	{
		terms: ['多P视频使用Season文件夹结构', '合集使用Season文件夹结构', '番剧使用统一Season文件夹结构'],
		description: '启用后按 Season 目录组织内容，提升 Emby/Jellyfin 识别兼容性。'
	},
	{
		terms: ['番剧文件名模板', '番剧文件夹名模板'],
		description: '分别控制番剧集文件名与番剧系列目录名称。'
	},
	{
		terms: ['合集文件夹模式', '合集/投稿目录模式', '分离模式', '统一模式', '投稿源同UP分季（仅投稿源）', '同UP合集分季', '合集统一命名模板'],
		description: '控制目录组织方式；分离/统一作用于合集与投稿，投稿源同UP分季仅作用于UP投稿源，不作用于独立合集源。'
	},
	{
		terms: ['显示变量说明', '隐藏变量说明'],
		description: '展开或收起模板变量帮助，查看可用字段与函数示例。'
	},
	{
		terms: ['NFO文件时间类型', '收藏时间', '发布时间'],
		description: '决定写入 NFO 的主时间字段，影响媒体库排序与展示时间。'
	},
	{
		terms: ['视频质量', '视频质量设置', '视频最高质量', '视频最低质量'],
		description: '控制视频流可选清晰度范围，超出范围会自动回退到可用档位。'
	},
	{
		terms: ['音频最高质量', '音频最低质量'],
		description: '控制音频流质量范围，确保在可用音轨内按偏好选择。'
	},
	{
		terms: ['8K超高清', '杜比视界', 'HDR真彩', '4K超高清', '1080P+高码率', '1080P高清', '720P高清', '480P清晰', '360P流畅'],
		description: '视频清晰度档位；按配置上限/下限与源站可用流综合选择。'
	},
	{
		terms: ['Hi-Res无损', '192K高品质', '杜比全景声', '132K标准', '64K省流'],
		description: '音频质量档位；优先选择高音质，缺失时回退到可用档位。'
	},
	{
		terms: ['编解码器优先级顺序'],
		description: '控制 AVC / HEV / AV1 的尝试顺序，按设备解码能力选择更稳策略。'
	},
	{
		terms: ['AVC', 'HEV', 'HEVC', 'AV1'],
		description: '视频编码格式：AVC 兼容高，HEV 压缩更优，AV1 压缩效率最高但解码要求更高。'
	},
	{
		terms: ['下载设置'],
		description: '配置下载器线程、并发与限流参数，平衡速度与稳定性。'
	},
	{
		terms: ['启用多线程下载', '下载线程数', '优先使用aria2'],
		description: '控制原生/aria2 下载实现与单任务分片并发强度。'
	},
	{
		terms: ['并发控制', '同时处理视频数', '每个视频并发分页数'],
		description: '控制任务并行度：跨视频并行和单视频分页并行。'
	},
	{
		terms: ['请求频率限制', '时间窗口'],
		description: '速率限制参数：在窗口期内限制请求总数，减少风控触发。'
	},
	{
		terms: ['弹幕设置'],
		description: '配置 ASS 弹幕样式，如字体、透明度、轨道比例和滚动时长。'
	},
	{
		terms: ['B站凭证', 'B站凭证设置'],
		description: '填写 B 站登录凭证；缺失或过期会导致接口能力受限。'
	},
	{
		terms: ['SESSDATA', 'bili_jct', 'buvid3', 'buvid4', 'DedeUserID', 'DedeUserID__ckMd5', 'ac_time_value'],
		description: 'B 站登录态关键字段，用于鉴权、设备指纹与风控校验。'
	},
	{
		terms: ['风控配置', '基础请求间隔（毫秒）', '大量视频UP主阈值', '大量视频延迟倍数'],
		description: '投稿扫描风控参数；通过延迟和阈值策略降低高频请求带来的限制风险。'
	},
	{
		terms: ['启用分批处理', '分批大小（页数）', '批次间延迟（秒）'],
		description: '将大批请求拆分执行并在批次间等待，降低瞬时请求密度。'
	},
	{
		terms: ['启用增量获取'],
		description: '投稿源优先增量扫描，减少重复请求和无效分页抓取。'
	},
	{
		terms: ['启用渐进式延迟', '自动退避基础时间（秒）', '自动退避最大倍数'],
		description: '连续失败时自动增加等待时间，缓解风控或临时网络波动。'
	},
	{
		terms: ['通用视频源间延迟（秒）', 'UP主投稿源间延迟（秒）'],
		description: '控制不同视频源扫描之间的等待时间，避免连续请求过密。'
	},
	{
		terms: ['验证码风控', '验证码风控设置', '启用风控验证', '验证模式', '验证超时时间（秒）'],
		description: '配置 v_voucher 风控时的处理方式：手动验证、自动识别或跳过验证。'
	},
	{
		terms: ['手动验证', '自动验证', '跳过验证'],
		description: '风控处理策略：手动完成验证码、第三方自动识别，或直接跳过本次验证。'
	},
	{
		terms: ['验证码服务', 'API密钥', '最大重试次数', '识别超时（秒）'],
		description: '自动验证码识别参数，决定第三方服务调用与重试行为。'
	},
	{
		terms: ['Aria2监控', 'Aria2监控设置', '启用Aria2健康检查', '健康检查间隔（秒）', '启用自动重启'],
		description: '下载器监控配置：检测 aria2 异常并按策略自动恢复。'
	},
	{
		terms: ['界面设置', '主题模式', '浅色模式', '深色模式', '跟随系统', '快速切换'],
		description: '管理界面外观与主题偏好设置。'
	},
	{
		terms: ['扫描间隔（秒）', '服务器端口', '绑定地址', '启用CDN排序', '显示已删除视频', 'UP主头像保存路径'],
		description: '系统级运行参数：扫描节奏、服务监听、CDN策略与展示选项。'
	},
	{
		terms: ['推送通知', '推送通知设置', '启用扫描完成推送通知', '通知渠道', '选择推送渠道'],
		description: '配置任务结果通知，在扫描完成或异常时推送消息到指定渠道。'
	},
	{
		terms: ['Server酱', 'Server酱3', '企业微信群机器人'],
		description: '可选通知渠道类型，分别使用对应平台的密钥或 webhook。'
	},
	{
		terms: ['消息格式', 'Markdown格式（推荐）', '纯文本格式'],
		description: '控制通知消息渲染格式，Markdown 信息量更高，纯文本兼容性更强。'
	},
	{
		terms: ['@所有人', '@特定成员（可选）'],
		description: '企业微信通知提及配置，可全体提醒或定向提醒成员。'
	},
	{
		terms: ['最小视频数阈值'],
		description: '新增视频数量达到阈值后才触发推送，避免低价值频繁通知。'
	},
	{
		terms: ['发送测试推送', '测试推送'],
		description: '发送一条测试消息，验证当前通知渠道配置是否可用。'
	},
	{
		terms: ['AI重命名', 'AI重命名设置', '启用AI重命名（全局开关）'],
		description: '使用大语言模型优化文件名；全局开启后还需在视频源级别启用。'
	},
	{
		terms: ['允许重命名上级目录（默认关闭）', '重命名上级目录'],
		description: '允许 AI 同步改名父目录；默认关闭以降低目录结构变更风险。'
	},
	{
		terms: ['API提供商', 'DeepSeek (付费API)', 'DeepSeek Web (免费)', '自定义 (OpenAI兼容)', '模型名称'],
		description: 'AI 命名服务配置：选择服务商、模型与接口兼容方式。'
	},
	{
		terms: ['请求超时时间（秒）', '视频重命名提示词（可选）', '音频重命名提示词（可选）'],
		description: '控制 AI 请求时限和命名指令内容，决定重命名风格与稳定性。'
	},
	{
		terms: ['对多P视频启用AI重命名', '对合集视频启用AI重命名', '对番剧启用AI重命名'],
		description: '控制 AI 重命名是否扩展到多P、合集、番剧等结构化内容。'
	},
	{
		terms: ['清除对话缓存', '清除全部缓存'],
		description: '清理 AI 会话缓存，强制后续请求重新建立上下文。'
	},
	{
		terms: ['日志文件', '导出日志', '下载日志文件', '全部日志', '信息', '警告', '错误', '调试'],
		description: '日志查看与导出功能：按文件和等级筛选并下载问题排查材料。'
	},
	{
		terms: ['当前任务状态', '扫描中', '空闲', '下次扫描', '开始运行', '运行结束'],
		description: '任务调度状态信息，显示本轮执行与下一轮计划时间。'
	},
	{
		terms: ['UP主ID', '合集ID', '收藏夹ID', 'Season ID', 'Media ID'],
		description: 'B站资源标识字段，用于唯一定位目标源、季度或媒体对象。'
	},
	{
		terms: ['保存路径', '基础保存路径', '当前路径'],
		description: '下载文件落盘目录；修改后会影响后续新下载文件的存放位置。'
	},
	{
		terms: ['下载选项', '下载所有季度', '合并选项（可选）', '合集类型', '视频源类型'],
		description: '控制新增视频源时的下载范围、组织方式和源类型行为。'
	},
	{
		terms: ['视频变量', '分页变量', '通用函数', '时间格式变量', '时间格式'],
		description: '命名模板可用的变量/函数说明，用于组合文件名与目录结构。'
	},
	{
		terms: ['弹幕持续时间（秒）', '宽度比例', '水平间距', '轨道高度', '滚动弹幕占比', '底部弹幕占比', '不透明度（0-255）', '描边宽度', '时间偏移（秒）', '字体', '字体大小', '加粗字体'],
		description: '弹幕样式参数，控制可读性、遮挡范围和时间同步效果。'
	},
	{
		terms: ['区分大小写', '白名单', '黑名单', '正则表达式示例'],
		description: '关键词过滤规则参数：决定匹配方式与优先级。'
	},
	{
		terms: ['反选', '全不选', '上一步', '上一页', '下一页', '末页', '详情'],
		description: '列表/分页操作按钮，用于切换选择范围或翻页查看内容。'
	},
	{
		terms: ['切换账号', '切换主题', '返回登录', '访问被拒绝'],
		description: '账号与界面入口操作；用于重新登录、切换主题或处理未授权状态。'
	},
	{
		terms: ['安装', '安装 bili-sync'],
		description: 'PWA 安装入口，可将管理页添加到桌面或移动设备主屏。'
	},
	{
		terms: ['成功', '失败', '未启用', '已删除', '已添加', '已修改', '已扫描', '暂无任务', '队列为空', '队列为空，没有待处理任务'],
		description: '任务状态提示词，用于反映当前操作结果或队列状态。'
	},
	{
		terms: ['提示', '重要提醒', '重要提醒：', '注意：', '功能说明', '使用说明', '常见任务类型', '其他设置说明', '主题说明'],
		description: '说明性文本块标题，用于解释配置影响、风险和推荐用法。'
	},
	{
		terms: ['最新入库', '近七日新增视频', '当前内存使用', '当前 CPU 使用率'],
		description: '首页统计指标，用于观察近期内容变化和运行资源占用。'
	},
	{
		terms: ['视频重命名提示词', '音频重命名提示词', '使用AI对下载的文件进行智能重命名', '自定义提示词（留空使用全局配置）'],
		description: 'AI 重命名控制项，用于定义视频/音频命名结果格式。'
	},
	{
		terms: ['Docker 路径说明', '如何获取B站登录凭证', '获取凭证信息', '方法一：通过开发者工具获取', '方法二：通过浏览器设置获取'],
		description: '操作指南类说明，帮助正确填写路径和登录凭证字段。'
	},
	{
		terms: ['投稿源扫描优化', '扫描摘要', '新增视频详情'],
		description: '扫描结果与优化信息展示区，用于查看本轮扫描产出和策略。'
	},
	{
		terms: ['使用建议', '路径重设说明', '故障排除', '路径冲突检测', '四步重命名原则说明'],
		description: '功能说明区块，提供推荐配置、风险提示和处理建议。'
	},
	{
		terms: ['名称', '类型', '内容管理', '监听源', '视频状态', '错误视频', '稍后'],
		description: '页面基础字段或分类标签，用于标记对象属性与状态。'
	},
	{
		terms: ['上次扫描', '下次运行', '加载失败', '等待数据', '请稍候'],
		description: '任务与数据加载状态提示。'
	},
	{
		terms: ['删除队列', '添加队列', '配置队列', '有任务排队，等待处理'],
		description: '队列处理状态信息，表示任务正在排队或等待执行。'
	},
	{
		terms: ['@所有人', '@特定成员'],
		description: '消息提及目标配置，可全体提醒或指定成员提醒。'
	},
	{
		terms: ['生成二维码', '设置B站登录凭证以启用视频下载功能', '请输入API Token以访问管理功能'],
		description: '首次接入与登录引导入口。'
	},
	{
		terms: ['搜索', '重试', '重新获取', '重新生成', '添加新视频源'],
		description: '常用操作按钮，用于查询、重试失败请求或新增资源。'
	},
	{
		terms: ['API Token', '设置 API Token', '设置 B站登录凭证', '凭证状态检查', '必填凭证已填写完整', '配置B站登录凭证信息'],
		description: '认证相关配置项，用于确保管理页访问和 B 站接口调用权限正常。'
	},
	{
		terms: ['危险操作警告', '文件删除警告', '重要说明', '确保数据完整性', '此操作不可恢复'],
		description: '风险提示信息，提醒你在删除、重置、迁移等操作前确认影响范围。'
	},
	{
		terms: ['智能路径管理', '路径冲突检测', '故障排除', '当前选择的季度', '推送内容示例', '分页状态', '下载进度', '性能提示'],
		description: '辅助信息区，用于说明路径策略、当前状态和功能示例。'
	},
	{
		terms: ['过滤逻辑说明', '并发控制说明', '弹幕设置说明', '多P视频Season结构说明', '番剧Season结构说明'],
		description: '说明类文案，解释该功能的处理逻辑、参数含义和推荐用法。'
	},
	{
		terms: ['定义视频文件的文件夹层级结构', '控制番剧的季度文件夹和集数文件名', '控制番剧主文件夹的命名，包含元数据文件', '控制时间变量的显示格式'],
		description: '命名与目录组织规则说明，影响文件路径、媒体库识别和排序表现。'
	},
	{
		terms: ['页面每5秒自动刷新状态', '加载队列状态中', '同类型任务按时间顺序依次执行', '扫描期间的所有操作会自动进入对应队列', '扫描完成后按优先级处理：配置 → 删除 → 添加'],
		description: '任务调度行为说明，帮助理解扫描中的排队、刷新和处理顺序。'
	},
	{
		terms: ['每个请求之间的基础延迟时间', '每个时间窗口内的最大请求数', '每批之间的等待时间', '退避时间的最大倍数限制', '渐进式延迟的最大倍数限制', '遇到错误时的基础等待时间', '遇到错误时自动增加延迟时间'],
		description: '请求节流与退避参数说明，用于平衡下载速度和风控稳定性。'
	},
	{
		terms: ['将大量请求分批处理，降低服务器压力', '优先获取最新视频，减少不必要的请求', '检测到下载器异常时自动重启实例', '定期检查下载器进程状态和RPC连接'],
		description: '下载稳定性优化策略说明，降低异常率并提升长时间运行可靠性。'
	},
	{
		terms: ['仅提取音频并转换为M4A格式，适合音乐类视频', '只下载匹配的视频（留空则不限制）', '只下载音频文件，不下载封面、NFO、弹幕、字幕', '下载CC字幕文件（SRT格式）'],
		description: '下载内容范围控制项，决定媒体文件类型和附加资源是否保存。'
	},
	{
		terms: ['点击下方按钮生成登录二维码', '点击右侧按钮获取您的收藏夹列表', '请按以下步骤获取B站登录凭证', '请输入绝对路径', '请修复配置错误后再保存', '跳过此步骤（稍后设置）'],
		description: '操作引导提示，按步骤完成登录凭证、路径和源配置。'
	},
	{
		terms: ['欢迎使用 bili-sync', 'bili-sync 管理页', 'API Token 用于保护管理界面的访问安全', 'Markdown格式支持富文本显示，纯文本更简洁'],
		description: '页面入口与基础说明信息，用于快速了解系统访问和通知格式能力。'
	},
	{
		terms: ['队列处理期间请避免频繁操作', '建议将验证超时时间设置为3-5分钟', '建议先小额充值测试服务稳定性', '每次重命名约消耗100-200个token', '配置更改会影响所有后续任务执行', '如果忘记可以在配置文件中查看或修改', '扫描进行时，手动操作会进入队列等待', '删除操作不可逆，请谨慎操作', '设置后请妥善保管，后续访问管理界面需要使用', '识别失败不会扣费，但重试会产生费用'],
		description: '使用建议与注意事项，帮助你在稳定性、成本和安全性之间取得平衡。'
	},
	{
		terms: ['所有人', '特定成员', '新下载的视频将自动使用AI生成的文件名', '完成验证后，程序会自动继续下载流程', '验证结果会缓存1小时，避免重复验证', '收藏夹 - 我的收藏', '验证收藏夹中', '超过此视频数量的UP主将启用风控策略', '此方法可最大程度避免文件丢失和名称冲突', '范围 1~168', '服务器监听地址和端口（修改后需要重启程序生效）', '该番剧只有当前一个季度', '勾选后将下载该番剧的所有季度，无需单独选择', '合并时自动沿用目标番剧源的名称', '合并选项', '开始重命名', '每个视频源之间的基础延迟时间（收藏夹、合集等）', '排除匹配的视频（优先级高于白名单）', '随着请求次数增加逐步延长延迟时间', '所有文件直接放入根目录，不创建子文件夹', '所有选中的视频源将保存到此路径', '跳转到第', '移动文件后，删除原始路径中的空文件夹', '优化下载节点选择', '在视频列表中显示已删除的视频'],
		description: '高级设置与流程提示文案，用于解释特殊策略、路径行为和扫描控制逻辑。'
	}
];

const NORMALIZE_REGEXP = /[\s:：()（）【】\[\]{}<>《》·•,，。.!！?？'"`~\/\\|]/g;
const TRAILING_SYMBOL_REGEXP = /[：:]+$|[.。…]+$/g;
const LEADING_SYMBOL_REGEXP = /^[^A-Za-z0-9\u4e00-\u9fff]+/g;
const STEP_PREFIX_REGEXP = /^(步骤\s*)?\d+[.、:：)\]]\s*/g;
const OPTIONAL_SUFFIX_REGEXP = /（(可选|推荐)）$/g;

function normalizeTerm(value: string): string {
	return value.toLowerCase().replace(NORMALIZE_REGEXP, '').trim();
}

function cleanupTerm(value: string): string {
	return value
		.replace(LEADING_SYMBOL_REGEXP, '')
		.replace(STEP_PREFIX_REGEXP, '')
		.replace(OPTIONAL_SUFFIX_REGEXP, '')
		.replace(TRAILING_SYMBOL_REGEXP, '')
		.trim();
}

const GLOSSARY_INDEX: GlossaryMatch[] = GLOSSARY_ENTRIES.flatMap((entry) =>
	entry.terms.map((term) => ({
		raw: term,
		normalized: normalizeTerm(term),
		description: entry.description
	}))
);

const EXACT_LOOKUP = new Map<string, GlossaryMatch>();
for (const item of GLOSSARY_INDEX) {
	if (!EXACT_LOOKUP.has(item.normalized)) {
		EXACT_LOOKUP.set(item.normalized, item);
	}
}

const PARTIAL_LOOKUP = [...EXACT_LOOKUP.values()]
	.filter((item) => item.normalized.length >= 4)
	.sort((a, b) => b.normalized.length - a.normalized.length);

const SEMANTIC_RESOLVERS: SemanticResolver[] = [
	{
		pattern: /^启用(.+)$/,
		resolve: (_term, match) => `开启「${match[1]}」功能，保存后会应用到后续任务。`
	},
	{
		pattern: /^禁用(.+)$/,
		resolve: (_term, match) => `关闭「${match[1]}」功能，后续任务将不再执行该能力。`
	},
	{
		pattern: /^清除(.+)$/,
		resolve: (_term, match) => `清空「${match[1]}」相关状态或筛选条件，恢复默认视图。`
	},
	{
		pattern: /^获取(.+)$/,
		resolve: (_term, match) => `向后端请求并加载「${match[1]}」数据。`
	},
	{
		pattern: /^搜索(.+)$/,
		resolve: (_term, match) => `按条件搜索「${match[1]}」并展示匹配结果。`
	},
	{
		pattern: /^选择(.+)$/,
		resolve: (_term, match) => `用于选择「${match[1]}」范围或目标对象。`
	},
	{
		pattern: /^查看(.+)$/,
		resolve: (_term, match) => `用于查看「${match[1]}」详细信息。`
	},
	{
		pattern: /^返回(.+)$/,
		resolve: (_term, match) => `返回到「${match[1]}」相关页面。`
	},
	{
		pattern: /^切换(.+)$/,
		resolve: (_term, match) => `切换「${match[1]}」当前状态或模式。`
	},
	{
		pattern: /^重新(.+)$/,
		resolve: (_term, match) => `重新执行「${match[1]}」操作。`
	},
	{
		pattern: /^设置(.+)$/,
		resolve: (_term, match) => `用于设置「${match[1]}」相关参数。`
	},
	{
		pattern: /^批量(.+)$/,
		resolve: (_term, match) => `对多个对象一次性执行「${match[1]}」操作。`
	},
	{
		pattern: /^重置(.+)$/,
		resolve: (_term, match) => `将「${match[1]}」状态恢复为待处理，以便系统重新执行。`
	},
	{
		pattern: /^(.+?)模板$/,
		resolve: (_term, match) => `定义「${match[1]}」的命名或目录格式，支持变量渲染。`
	},
	{
		pattern: /^(.+?)说明$/,
		resolve: (_term, match) => `「${match[1]}」的功能说明，用于帮助理解配置含义和影响范围。`
	},
	{
		pattern: /^(.+?)警告$/,
		resolve: (_term, match) => `「${match[1]}」风险提示，请在执行前确认影响范围。`
	},
	{
		pattern: /^(.+?)示例$/,
		resolve: (_term, match) => `「${match[1]}」示例内容，用于预览实际展示效果。`
	},
	{
		pattern: /^(.+?)已启用$/,
		resolve: (_term, match) => `表示「${match[1]}」功能当前已启用。`
	},
	{
		pattern: /^(.+?)已禁用$/,
		resolve: (_term, match) => `表示「${match[1]}」功能当前已禁用。`
	},
	{
		pattern: /^(.+?)ID$/,
		resolve: (_term, match) => `「${match[1]}」唯一标识，用于定位目标资源。`
	},
	{
		pattern: /^(.+?)模式$/,
		resolve: (_term, match) => `用于切换「${match[1]}」的工作策略。`
	},
	{
		pattern: /^(.+?)（(秒|毫秒|分钟|小时|页数|个)）$/,
		resolve: (_term, match) => `用于控制「${match[1]}」的数值参数，单位为${match[2]}。`
	},
	{
		pattern: /^(.+?)(设置|配置)$/,
		resolve: (_term, match) => `用于调整「${match[1]}」相关参数。`
	},
	{
		pattern: /^(.+?)用于(.+)$/,
		resolve: (_term, match) => `说明「${match[1]}」的用途：${match[2]}。`
	},
	{
		pattern: /^点击(.+)$/,
		resolve: (_term, match) => `操作引导：${match[1]}。`
	},
	{
		pattern: /^请(.+)$/,
		resolve: (_term, match) => `操作提示：${match[1]}。`
	},
	{
		pattern: /^建议(.+)$/,
		resolve: (_term, match) => `推荐做法：${match[1]}。`
	},
	{
		pattern: /^控制(.+)$/,
		resolve: (_term, match) => `用于控制${match[1]}。`
	},
	{
		pattern: /^定义(.+)$/,
		resolve: (_term, match) => `用于定义${match[1]}。`
	},
	{
		pattern: /^处理(.+)$/,
		resolve: (_term, match) => `用于处理${match[1]}。`
	},
	{
		pattern: /^将(.+)$/,
		resolve: (_term, match) => `执行后将${match[1]}。`
	},
	{
		pattern: /^仅(.+)$/,
		resolve: (_term, match) => `仅执行：${match[1]}。`
	},
	{
		pattern: /^只(.+)$/,
		resolve: (_term, match) => `仅执行：${match[1]}。`
	},
	{
		pattern: /^扫描耗时[:：]\s*(.+)$/,
		resolve: (_term, match) => `本轮扫描耗时统计：${match[1]}。`
	},
	{
		pattern: /^扫描视频源[:：]\s*(\d+)个$/,
		resolve: (_term, match) => `本轮扫描的视频源数量：${match[1]} 个。`
	},
	{
		pattern: /^新增视频[:：]\s*(\d+)个$/,
		resolve: (_term, match) => `本轮新发现视频数量：${match[1]} 个。`
	},
	{
		pattern: /^视频标题\d+\s*\((.+)\)$/,
		resolve: () => '扫描结果中的示例视频标题，用于预览新增内容。'
	},
	{
		pattern: /^0表示不限制(.+)$/,
		resolve: (_term, match) => `参数值 0 表示不限制；${match[1]}。`
	},
	{
		pattern: /^0\s*表示不限制(.+)$/,
		resolve: (_term, match) => `参数值 0 表示不限制；${match[1]}。`
	},
	{
		pattern: /^验证(.+)中$/,
		resolve: (_term, match) => `系统正在验证「${match[1]}」，请稍候。`
	},
	{
		pattern: /^范围\s*(\d+~\d+)$/,
		resolve: (_term, match) => `可配置数值范围：${match[1]}。`
	},
	{
		pattern: /^收藏夹\s*-\s*(.+)$/,
		resolve: (_term, match) => `收藏夹扫描摘要：${match[1]}。`
	},
	{
		pattern: /^(.+?)筛选$/,
		resolve: (_term, match) => `按「${match[1]}」条件过滤列表结果。`
	},
	{
		pattern: /^(.+?)(间隔|延迟|超时|阈值|倍数|并发|线程数|页数|数量)$/,
		resolve: (_term, match) => `用于控制「${match[1]}」的${match[2]}参数。`
	},
	{
		pattern: /^每页$/,
		resolve: () => '控制单页展示的数据条数。'
	},
	{
		pattern: /^每行$/,
		resolve: () => '控制网格每行显示的卡片数量。'
	},
	{
		pattern: /^暂无(.+)$/,
		resolve: (_term, match) => `当前没有可用的「${match[1]}」数据。`
	},
	{
		pattern: /^正在(.+)$/,
		resolve: (_term, match) => `系统正在执行「${match[1]}」，请稍候。`
	},
	{
		pattern: /^已(.+)$/,
		resolve: (_term, match) => `表示该项「${match[1]}」状态已生效。`
	},
	{
		pattern: /^成功$/,
		resolve: () => '操作已成功完成。'
	},
	{
		pattern: /^失败$/,
		resolve: () => '操作执行失败，请查看日志或错误提示。'
	},
	{
		pattern: /^(\d{3,4})p$/i,
		resolve: (_term, match) => `分辨率档位（约 ${match[1]}p 高度）。`
	}
];

function getExactOrPartialMatch(normalizedInput: string): GlossaryMatch | null {
	const exact = EXACT_LOOKUP.get(normalizedInput);
	if (exact) return exact;

	for (const item of PARTIAL_LOOKUP) {
		if (
			normalizedInput.startsWith(item.normalized) ||
			normalizedInput.endsWith(item.normalized) ||
			normalizedInput.includes(item.normalized)
		) {
			return item;
		}
	}

	return null;
}

function inferSemanticDescription(cleanTerm: string): string | null {
	for (const resolver of SEMANTIC_RESOLVERS) {
		const matched = cleanTerm.match(resolver.pattern);
		if (matched) {
			return resolver.resolve(cleanTerm, matched);
		}
	}
	return null;
}

export function getGlossaryDescription(term: string): string | null {
	const cleanTerm = cleanupTerm(term);
	if (!cleanTerm) return null;

	const normalizedInput = normalizeTerm(cleanTerm);
	if (!normalizedInput) return null;

	const exactOrPartial = getExactOrPartialMatch(normalizedInput);
	if (exactOrPartial) {
		return `${exactOrPartial.raw}：${exactOrPartial.description}`;
	}

	const semantic = inferSemanticDescription(cleanTerm);
	if (semantic) {
		return `${cleanTerm}：${semantic}`;
	}

	return null;
}
