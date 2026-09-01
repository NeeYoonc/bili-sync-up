use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::deepseek_web::DeepSeekSession;
use bili_sync_entity::ai_conversation_history;

/// AI 重命名上下文（从 API 获取的视频信息）
#[derive(Clone, Debug, Default)]
pub struct AiRenameContext {
    /// 视频标题
    pub title: String,
    /// 视频简介
    pub desc: String,
    /// UP主名称
    pub owner: String,
    /// 分区名称
    pub tname: String,
    /// 时长（秒）
    pub duration: u32,
    /// 发布日期（如 "2023-12-29"）
    pub pubdate: String,
    /// 分辨率（如 "1920x1080"）
    pub dimension: String,
    /// 当前分P名称
    pub part_name: String,
    /// 合集名称（如果属于合集）
    pub ugc_season: Option<String>,
    /// 版权类型（"自制" 或 "转载"）
    pub copyright: String,
    /// 播放量
    pub view: u64,
    /// 当前是第几P
    pub pid: i32,
    /// 合集中第几集
    pub episode_number: Option<i32>,
    /// 来源类型（收藏夹/合集/投稿等）
    pub source_type: String,
    /// 是否为音频模式
    pub is_audio: bool,
    /// 在视频源中的排序位置（按发布时间排序后的顺序，从1开始）
    pub sort_index: Option<i32>,
    /// B站视频ID（BV号）
    pub bvid: String,
    /// 是否为多P视频（多个分页）
    pub is_multi_page: bool,
    /// 是否为番剧
    pub is_bangumi: bool,
    /// 目录结构描述（如 "Season 结构" / "合集统一模式" / "UP分季结构"；空表示无特殊结构）
    pub structure: String,
}

impl AiRenameContext {
    /// 构建发送给 AI 的 JSON 信息
    pub fn to_json_string(&self) -> String {
        let mut info = serde_json::json!({
            "标题": self.title,
            "UP主": self.owner,
            "来源": self.source_type,
        });

        // 添加BV号（非空时）
        if !self.bvid.is_empty() {
            info["BV号"] = serde_json::json!(self.bvid);
        }

        // 只添加非空字段
        if !self.tname.is_empty() {
            info["分区"] = serde_json::json!(self.tname);
        }
        if !self.dimension.is_empty() {
            info["清晰度"] = serde_json::json!(self.dimension);
        }
        if self.duration > 0 {
            let dur_str = if self.duration >= 3600 {
                format!(
                    "{}:{:02}:{:02}",
                    self.duration / 3600,
                    (self.duration % 3600) / 60,
                    self.duration % 60
                )
            } else {
                format!("{}:{:02}", self.duration / 60, self.duration % 60)
            };
            info["时长"] = serde_json::json!(dur_str);
        }
        if !self.pubdate.is_empty() {
            info["发布日期"] = serde_json::json!(self.pubdate);
        }
        if !self.copyright.is_empty() {
            info["版权"] = serde_json::json!(self.copyright);
        }
        if self.view > 0 {
            info["播放量"] = serde_json::json!(self.view);
        }
        if let Some(ref season) = self.ugc_season {
            info["合集"] = serde_json::json!(season);
        }
        if let Some(ep) = self.episode_number {
            info["集数"] = serde_json::json!(format!("第{}集", ep));
        }
        if let Some(idx) = self.sort_index {
            info["排序位置"] = serde_json::json!(idx);
        }
        if self.pid > 1 {
            info["分P"] = serde_json::json!(format!("P{}", self.pid));
        }
        if !self.part_name.is_empty() && self.part_name != self.title {
            info["分P名称"] = serde_json::json!(self.part_name);
        }
        if !self.desc.is_empty() {
            // 简介截取前200字符
            let desc_short = if self.desc.chars().count() > 200 {
                format!("{}...", self.desc.chars().take(200).collect::<String>())
            } else {
                self.desc.clone()
            };
            info["简介"] = serde_json::json!(desc_short);
        }
        if self.is_audio {
            info["模式"] = serde_json::json!("仅音频");
        }
        // 类型标记：帮助 AI 识别单P/多P/番剧，从而生成与文件一一对应的文件名
        if self.is_bangumi {
            info["类型"] = serde_json::json!("番剧");
        } else if self.is_multi_page {
            info["类型"] = serde_json::json!("多P视频");
        } else {
            info["类型"] = serde_json::json!("单P视频");
        }
        if !self.structure.is_empty() {
            info["目录结构"] = serde_json::json!(self.structure);
        }

        let json_str = serde_json::to_string_pretty(&info).unwrap_or_default();

        // 添加 API 参考链接，让 AI 可以参考更多信息
        if !self.bvid.is_empty() {
            format!(
                "{}\nAPI参考: https://api.bilibili.com/x/web-interface/view?bvid={}",
                json_str, self.bvid
            )
        } else {
            json_str
        }
    }
}

/// DeepSeek Web 会话缓存（按 source_key 存储）
/// 同一个视频源复用同一个会话，避免创建过多会话
/// 使用 tokio::sync::Mutex 确保异步安全
static DEEPSEEK_SESSION_CACHE: Lazy<Mutex<HashMap<String, DeepSeekSession>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// AI 重命名全局锁（确保同一时间只有一个 AI 重命名请求）
/// 防止并发请求导致创建多个会话
static AI_RENAME_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// 对话消息（用于存储历史）
#[derive(Clone, Debug)]
struct ConversationMessage {
    role: String,
    content: String,
}

/// 清除指定视频源的对话历史（数据库持久化版本）
pub async fn clear_naming_cache(source_key: &str) -> Result<()> {
    let db = crate::database::get_global_db().ok_or_else(|| anyhow!("数据库连接不可用"))?;

    let result = ai_conversation_history::Entity::delete_many()
        .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
        .exec(db.as_ref())
        .await?;

    // 同时清除 DeepSeek Web 会话缓存
    {
        let mut cache = DEEPSEEK_SESSION_CACHE.lock().await;
        cache.remove(source_key);
    }

    info!(
        "已清除视频源 {} 的AI对话历史，删除 {} 条记录",
        source_key, result.rows_affected
    );
    Ok(())
}

/// 清除所有对话历史（数据库持久化版本）
pub async fn clear_all_naming_cache() -> Result<()> {
    let db = crate::database::get_global_db().ok_or_else(|| anyhow!("数据库连接不可用"))?;

    let result = ai_conversation_history::Entity::delete_many().exec(db.as_ref()).await?;

    // 同时清除所有 DeepSeek Web 会话缓存
    {
        let mut cache = DEEPSEEK_SESSION_CACHE.lock().await;
        cache.clear();
    }

    info!("已清除所有AI对话历史，删除 {} 条记录", result.rows_affected);
    Ok(())
}

/// 添加对话消息到历史（数据库持久化版本）
async fn add_conversation_message(db: &DatabaseConnection, source_key: &str, role: &str, content: &str) -> Result<()> {
    // 获取当前最大的order_index
    let max_order = ai_conversation_history::Entity::find()
        .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
        .order_by_desc(ai_conversation_history::Column::OrderIndex)
        .one(db)
        .await?
        .map(|m| m.order_index)
        .unwrap_or(-1);

    let new_order = max_order + 1;

    // 检查消息数量，如果超过10条（5轮对话）则删除最早的2条
    let count = ai_conversation_history::Entity::find()
        .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
        .count(db)
        .await?;

    if count >= 10 {
        // 获取最早的2条记录的ID
        let oldest = ai_conversation_history::Entity::find()
            .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
            .order_by_asc(ai_conversation_history::Column::OrderIndex)
            .limit(2)
            .all(db)
            .await?;

        for record in oldest {
            ai_conversation_history::Entity::delete_by_id(record.id)
                .exec(db)
                .await?;
        }
        debug!("清理 {} 的旧对话记录，保留最近8条", source_key);
    }

    // 插入新消息
    let new_message = ai_conversation_history::ActiveModel {
        source_key: Set(source_key.to_string()),
        role: Set(role.to_string()),
        content: Set(content.to_string()),
        order_index: Set(new_order),
        created_at: Set(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        ..Default::default()
    };

    new_message.insert(db).await?;
    debug!(
        "保存对话消息到数据库: source_key={}, role={}, order={}",
        source_key, role, new_order
    );

    Ok(())
}

/// 获取对话历史（数据库持久化版本）
async fn get_conversation_history(db: &DatabaseConnection, source_key: &str) -> Vec<ConversationMessage> {
    match ai_conversation_history::Entity::find()
        .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
        .order_by_asc(ai_conversation_history::Column::OrderIndex)
        .all(db)
        .await
    {
        Ok(records) => records
            .into_iter()
            .map(|r| ConversationMessage {
                role: r.role,
                content: r.content,
            })
            .collect(),
        Err(e) => {
            warn!("获取对话历史失败: {}", e);
            Vec::new()
        }
    }
}

/// 保存 DeepSeek 会话到数据库
/// 使用 role = "deepseek_session" 标识，content 存储 JSON
async fn save_deepseek_session(db: &DatabaseConnection, source_key: &str, session: &DeepSeekSession) -> Result<()> {
    // 序列化会话信息
    let content = serde_json::to_string(session)?;

    // 先删除旧的会话记录
    ai_conversation_history::Entity::delete_many()
        .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
        .filter(ai_conversation_history::Column::Role.eq("deepseek_session"))
        .exec(db)
        .await?;

    // 插入新记录
    let new_record = ai_conversation_history::ActiveModel {
        source_key: Set(source_key.to_string()),
        role: Set("deepseek_session".to_string()),
        content: Set(content),
        order_index: Set(-1), // 特殊标记
        created_at: Set(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        ..Default::default()
    };
    new_record.insert(db).await?;

    debug!(
        "保存 DeepSeek 会话到数据库: source_key={}, session_id={}",
        source_key, session.session_id
    );
    Ok(())
}

/// 从数据库加载 DeepSeek 会话
async fn load_deepseek_session(db: &DatabaseConnection, source_key: &str) -> Option<DeepSeekSession> {
    match ai_conversation_history::Entity::find()
        .filter(ai_conversation_history::Column::SourceKey.eq(source_key))
        .filter(ai_conversation_history::Column::Role.eq("deepseek_session"))
        .one(db)
        .await
    {
        Ok(Some(record)) => match serde_json::from_str::<DeepSeekSession>(&record.content) {
            Ok(session) => {
                debug!(
                    "从数据库加载 DeepSeek 会话: source_key={}, session_id={}",
                    source_key, session.session_id
                );
                Some(session)
            }
            Err(e) => {
                warn!("解析 DeepSeek 会话失败: {}", e);
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            warn!("加载 DeepSeek 会话失败: {}", e);
            None
        }
    }
}

/// AI 重命名全局配置（存储在 Config 中）
///
/// 说明：这里走 **OpenAI 兼容** 的 chat/completions 接口（DeepSeek / OpenAI / 其它兼容服务都可）。
/// 如果 api_key 为空，会直接返回错误，由调用方决定是否跳过。
///
/// 当 provider 为 "deepseek-web" 时，使用 chat.deepseek.com 免费 Web API。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiRenameConfig {
    /// 是否启用 AI 重命名（全局开关）
    pub enabled: bool,
    /// Provider 类型（openai / deepseek / deepseek-web / custom）
    /// - openai/deepseek/custom: 使用 OpenAI 兼容 API
    /// - deepseek-web: 使用 chat.deepseek.com 免费 Web API
    pub provider: String,
    /// OpenAI 兼容接口 base url，例如：
    /// - https://api.openai.com/v1
    /// - https://api.deepseek.com/v1
    pub base_url: String,
    /// API Key（用户自备）- 用于 OpenAI 兼容 API
    pub api_key: Option<String>,
    /// DeepSeek Web Token - 用于 deepseek-web provider
    /// 从浏览器开发者工具中获取
    #[serde(default)]
    pub deepseek_web_token: Option<String>,
    /// 模型名，例如 deepseek-v4-flash / gpt-4o-mini
    pub model: String,
    /// 请求超时（秒）
    pub timeout_seconds: u64,
    /// 视频提示词（不含扩展名）
    pub video_prompt_hint: String,
    /// 音频提示词（不含扩展名）
    pub audio_prompt_hint: String,
    /// 是否启用多P视频AI重命名（默认关闭）
    #[serde(default)]
    pub enable_multi_page: bool,
    /// 是否启用合集视频AI重命名（默认关闭）
    #[serde(default)]
    pub enable_collection: bool,
    /// 是否启用番剧AI重命名（默认关闭）
    #[serde(default)]
    pub enable_bangumi: bool,
    /// 是否允许重命名上级目录（默认关闭）
    #[serde(default)]
    pub rename_parent_dir: bool,
}

impl Default for AiRenameConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "custom".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: None,
            deepseek_web_token: None,
            model: "deepseek-v4-flash".to_string(),
            timeout_seconds: 20,
            // 视频命名规则
            video_prompt_hint: "【命名结构】精简标题-作者-时间(YYYYMMDD)；\
【标题规则】仅保留核心主题，去除修饰性/情绪性/营销性词语，不使用表情；\
【符号规则】仅用英文连字符-，禁止其他特殊符号"
                .to_string(),
            // 音频命名规则
            audio_prompt_hint: "【命名结构】歌手-歌名-版本(如录音棚/现场)-时间(YYYYMMDD)；\
【规则】去除表情/情绪文案，仅用英文连字符-连接"
                .to_string(),
            // 特殊类型默认关闭
            enable_multi_page: false,
            enable_collection: false,
            enable_bangumi: false,
            rename_parent_dir: false,
        }
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    mode: String,
}

#[derive(Serialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessageResponse,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// 重命名同目录下的侧车文件（nfo/xml/srt/jpg/jpeg/png/ass等）
/// 支持复杂后缀如 -fanart.jpg, -thumb.jpg, .zh-CN.default.ass 等
/// 重命名侧车文件（NFO、字幕、封面等）
///
/// # 参数
/// - `old`: 原始文件路径（用于获取目录和原文件名基）
/// - `new_stem`: 新的文件名基（不含扩展名）
/// - `new_ext`: 新的主文件扩展名（用于排除已重命名的主文件）
pub fn rename_sidecars(old: &Path, new_stem: &str, new_ext: &str) -> Result<()> {
    let parent = old.parent().ok_or_else(|| anyhow!("Invalid path"))?;
    let stem = old
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Invalid stem"))?;

    // 新的主文件名（需要跳过）
    let new_main_filename = format!("{}.{}", new_stem, new_ext);

    // 扫描目录中所有以旧文件名stem开头的文件
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let filename = match path.file_name().and_then(|s| s.to_str()) {
                Some(f) => f,
                None => continue,
            };

            // 跳过刚重命名的主文件
            if filename == new_main_filename {
                continue;
            }

            // 检查文件名是否以旧stem开头
            if !filename.starts_with(stem) {
                continue;
            }

            // 获取stem之后的后缀部分（如 "-fanart.jpg", ".nfo", ".zh-CN.default.ass"）
            let suffix = &filename[stem.len()..];

            // 跳过原始主视频/音频文件本身（理论上已经被重命名了，但以防万一）
            if suffix.starts_with('.') {
                let ext_lower = suffix.to_lowercase();
                if ext_lower == ".mp4"
                    || ext_lower == ".mkv"
                    || ext_lower == ".m4a"
                    || ext_lower == ".flv"
                    || ext_lower == ".webm"
                    || ext_lower == ".avi"
                {
                    continue;
                }
            }

            // 构建新文件名
            let new_filename = format!("{}{}", new_stem, suffix);
            let new_path = parent.join(&new_filename);

            // 如果新路径已存在则跳过
            if new_path.exists() {
                warn!("侧车文件目标已存在，跳过: {} -> {}", filename, new_filename);
                continue;
            }

            // 执行重命名
            if let Err(e) = fs::rename(&path, &new_path) {
                warn!("重命名侧车文件失败 {} -> {}: {}", filename, new_filename, e);
            } else {
                info!("重命名侧车文件: {} -> {}", filename, new_filename);
            }
        }
    }

    Ok(())
}

/// 更新NFO文件中的标题标签
///
/// # 参数
/// - `nfo_path`: NFO文件路径
/// - `new_title`: 新的标题（文件名stem）
///
/// # 说明
/// 更新NFO文件中的 <title>、<originaltitle>、<sorttitle> 标签
/// 如果新标题为空，则跳过不修改
pub fn update_nfo_content(nfo_path: &Path, new_title: &str) -> Result<()> {
    // 如果新标题为空，跳过不修改
    if new_title.trim().is_empty() {
        debug!("跳过NFO更新: 新标题为空");
        return Ok(());
    }

    // 检查NFO文件是否存在
    if !nfo_path.exists() {
        debug!("跳过NFO更新: 文件不存在 {:?}", nfo_path);
        return Ok(());
    }

    // 读取NFO文件内容
    let content = match fs::read_to_string(nfo_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("读取NFO文件失败 {:?}: {}", nfo_path, e);
            return Err(anyhow!("读取NFO文件失败: {}", e));
        }
    };

    // 使用正则表达式更新标签内容
    // 匹配 <title>...</title>、<originaltitle>...</originaltitle>、<sorttitle>...</sorttitle>
    let title_re = Regex::new(r"<title>([^<]*)</title>").unwrap();
    let originaltitle_re = Regex::new(r"<originaltitle>([^<]*)</originaltitle>").unwrap();
    let sorttitle_re = Regex::new(r"<sorttitle>([^<]*)</sorttitle>").unwrap();

    let mut updated_content = content.clone();
    let mut updated = false;

    // 更新 <title> 标签（仅当原内容非空时）
    if let Some(caps) = title_re.captures(&content) {
        if !caps.get(1).map_or(true, |m| m.as_str().trim().is_empty()) {
            updated_content = title_re
                .replace(&updated_content, format!("<title>{}</title>", new_title))
                .to_string();
            updated = true;
        }
    }

    // 更新 <originaltitle> 标签（仅当原内容非空时）
    if let Some(caps) = originaltitle_re.captures(&updated_content) {
        if !caps.get(1).map_or(true, |m| m.as_str().trim().is_empty()) {
            updated_content = originaltitle_re
                .replace(
                    &updated_content,
                    format!("<originaltitle>{}</originaltitle>", new_title),
                )
                .to_string();
            updated = true;
        }
    }

    // 更新 <sorttitle> 标签（仅当原内容非空时）
    if let Some(caps) = sorttitle_re.captures(&updated_content) {
        if !caps.get(1).map_or(true, |m| m.as_str().trim().is_empty()) {
            updated_content = sorttitle_re
                .replace(&updated_content, format!("<sorttitle>{}</sorttitle>", new_title))
                .to_string();
            updated = true;
        }
    }

    // 如果有更新，写回文件
    if updated {
        if let Err(e) = fs::write(nfo_path, &updated_content) {
            warn!("写入NFO文件失败 {:?}: {}", nfo_path, e);
            return Err(anyhow!("写入NFO文件失败: {}", e));
        }
        info!("更新NFO文件标题: {:?} -> {}", nfo_path, new_title);
    } else {
        debug!("NFO文件无需更新（标签为空或不存在）: {:?}", nfo_path);
    }

    Ok(())
}

/// 批量重命名结果
#[derive(Debug, Default)]
pub struct BatchRenameResult {
    pub renamed_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
}

/// 待重命名的文件信息
#[derive(Clone)]
pub struct FileToRename {
    /// 原始文件路径
    pub path: std::path::PathBuf,
    /// 当前文件名（不含扩展名）
    pub current_stem: String,
    /// 扩展名
    pub ext: String,
    /// AI 上下文
    pub ctx: AiRenameContext,
    /// page.id（用于更新数据库）
    pub page_id: i32,
    /// video.id（用于更新数据库）
    pub video_id: i32,
    /// video bvid（用于冲突时追加）
    pub bvid: String,
    /// 是否为单P视频
    pub single_page: bool,
    /// 是否为平铺目录模式（启用时不重命名子文件夹）
    pub flat_folder: bool,
}

/// 待重命名的视频文件夹（多P视频独占文件夹）
#[derive(Clone)]
pub struct FolderToRename {
    /// 视频文件夹路径
    pub path: std::path::PathBuf,
    /// 当前文件夹名
    pub current_name: String,
    /// AI 上下文（视频级信息）
    pub ctx: AiRenameContext,
    /// video.id（用于更新数据库）
    pub video_id: i32,
    /// bvid（冲突时追加）
    pub bvid: String,
}

/// 批量生成文件名（一次请求处理多个文件）
///
/// # 参数
/// - `cfg`: AI 重命名配置
/// - `source_key`: 视频源标识
/// - `files`: 待重命名的文件列表
/// - `prompt_hint`: 命名提示词
///
/// # 返回
/// - 新文件名列表（与输入顺序对应）
/// 构建批量文件名的 AI prompt（纯函数，便于测试）
///
/// 每个文件条目包含序号、当前文件名和带类型/结构标记的视频信息，
/// 并明确要求 AI 返回与文件一一对应的文件名。
fn build_batch_filenames_prompt(files: &[FileToRename], prompt_hint: &str) -> String {
    let mut file_list = String::new();
    for (i, file) in files.iter().enumerate() {
        let video_info = file.ctx.to_json_string();
        file_list.push_str(&format!(
            "{}. 当前文件名: {}\n   视频信息: {}\n\n",
            i + 1,
            file.current_stem,
            video_info.replace('\n', " ") // 压缩为单行
        ));
    }

    format!(
        "请为以下 {} 个视频文件生成新的文件名。\n\
        【重要】返回的文件名数量必须与文件数量完全一致（{} 个），第 N 个文件名必须与第 N 个文件一一对应，不得打乱顺序、不得合并、不得遗漏任何一个文件！\n\
        【重要】多P/番剧等带序号结构的文件，生成的名字必须保留原有序号信息（如 P1/P2 或 S01E01），用于区分同一视频的不同分页/剧集；不同类型（单P/多P/番剧）按各自类型生成对应格式的文件名。\n\
        【重要】严格按照用户指定的命名格式生成，不要添加格式中未要求的任何信息！\n\
        严格按照 JSON 数组格式返回，只输出文件名（不含扩展名），不要解释：\n\
        [\"文件名1\", \"文件名2\", ...]\n\n\
        用户指定的命名格式：{}\n\n\
        文件列表（仅供参考，只提取格式中需要的字段；如信息缺失可访问API参考链接获取）：\n{}",
        files.len(),
        files.len(),
        prompt_hint,
        file_list
    )
}

pub async fn ai_generate_filenames_batch(
    cfg: &AiRenameConfig,
    source_key: &str,
    files: &[FileToRename],
    prompt_hint: &str,
) -> Result<Vec<String>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let full_prompt = build_batch_filenames_prompt(files, prompt_hint);

    // 根据 provider 选择实现
    if cfg.provider == "deepseek-web" {
        ai_generate_filenames_batch_deepseek_web(cfg, source_key, &full_prompt, files.len()).await
    } else {
        ai_generate_filenames_batch_openai(cfg, source_key, &full_prompt, files.len()).await
    }
}

/// 批量生成视频文件夹名（一次请求处理多个文件夹）
///
/// 用于多P视频独占文件夹的 AI 重命名：输入为视频级信息，
/// 输出为新的文件夹名（不含扩展名、不含路径分隔符）。
pub async fn ai_generate_folder_names_batch(
    cfg: &AiRenameConfig,
    source_key: &str,
    folders: &[FolderToRename],
    prompt_hint: &str,
) -> Result<Vec<String>> {
    if folders.is_empty() {
        return Ok(Vec::new());
    }

    let mut folder_list = String::new();
    for (i, folder) in folders.iter().enumerate() {
        let video_info = folder.ctx.to_json_string();
        folder_list.push_str(&format!(
            "{}. 当前文件夹名: {}\n   视频信息: {}\n\n",
            i + 1,
            folder.current_name,
            video_info.replace('\n', " ")
        ));
    }

    let full_prompt = format!(
        "请为以下 {} 个视频的文件夹生成新的文件夹名（是整个视频的文件夹，不是单个文件）。\n\
        【重要】返回的文件夹名数量必须与文件夹数量完全一致（{} 个），第 N 个文件夹名必须与第 N 个文件夹一一对应，不得打乱顺序、不得遗漏任何一个文件夹！\n\
        【重要】多P/番剧等视频文件夹的命名，需要与内部分页/剧集文件配套，保留必要的序号语义。\n\
        【重要】严格按照用户指定的命名格式生成，不要添加格式中未要求的任何信息！\n\
        【重要】只输出文件夹名本身，不要包含路径分隔符（/ 或 \\\\），不要包含扩展名！\n\
        严格按照 JSON 数组格式返回，只输出文件夹名，不要解释：\n\
        [\"文件夹名1\", \"文件夹名2\", ...]\n\n\
        用户指定的命名格式：{}\n\n\
        文件夹列表（仅供参考，只提取格式中需要的字段；如信息缺失可访问API参考链接获取）：\n{}",
        folders.len(),
        folders.len(),
        prompt_hint,
        folder_list
    );

    // 根据 provider 选择实现（复用文件名的生成逻辑，prompt 已按文件夹名构造）
    if cfg.provider == "deepseek-web" {
        ai_generate_filenames_batch_deepseek_web(cfg, source_key, &full_prompt, folders.len()).await
    } else {
        ai_generate_filenames_batch_openai(cfg, source_key, &full_prompt, folders.len()).await
    }
}

/// DeepSeek Web 批量生成
async fn ai_generate_filenames_batch_deepseek_web(
    cfg: &AiRenameConfig,
    source_key: &str,
    prompt: &str,
    expected_count: usize,
) -> Result<Vec<String>> {
    let _lock = AI_RENAME_LOCK.lock().await;

    let token = cfg
        .deepseek_web_token
        .clone()
        .ok_or_else(|| anyhow!("DeepSeek Web Token 未配置"))?;

    let db = crate::database::get_global_db().ok_or_else(|| anyhow!("数据库连接不可用"))?;

    // 从缓存获取会话
    let cached_session = {
        let cache = DEEPSEEK_SESSION_CACHE.lock().await;
        if let Some(session) = cache.get(source_key).cloned() {
            info!(
                "会话缓存命中（内存）: source_key='{}', session_id='{}'",
                source_key, session.session_id
            );
            Some(session)
        } else {
            drop(cache);
            if let Some(session) = load_deepseek_session(db.as_ref(), source_key).await {
                info!(
                    "会话缓存命中（数据库）: source_key='{}', session_id='{}'",
                    source_key, session.session_id
                );
                let mut cache = DEEPSEEK_SESSION_CACHE.lock().await;
                cache.insert(source_key.to_string(), session.clone());
                Some(session)
            } else {
                info!("会话缓存未命中: source_key='{}'，将创建新会话", source_key);
                None
            }
        }
    };

    // 调用 DeepSeek Web API（使用原始响应，不清洗）
    let result =
        super::deepseek_web::deepseek_web_generate_raw(&token, cached_session, prompt, cfg.timeout_seconds).await;

    // 检查是否是会话长度上限错误，需要重建会话
    let (response, new_session) = match result {
        Ok(res) => res,
        Err(e) if e.to_string().contains("SESSION_LIMIT_REACHED") => {
            warn!("[{}] DeepSeek 会话达到长度上限，正在重建会话并带上历史...", source_key);

            // 清除旧会话缓存
            {
                let mut cache = DEEPSEEK_SESSION_CACHE.lock().await;
                cache.remove(source_key);
            }

            // 获取历史记录作为上下文
            let history = get_conversation_history(db.as_ref(), source_key).await;
            let history_context = if !history.is_empty() {
                let mut ctx = String::from("【之前的命名风格参考】\n");
                for msg in &history {
                    if msg.role == "assistant" {
                        // 只保留 assistant 的回复作为命名风格参考
                        ctx.push_str(&format!("{}\n", msg.content));
                    }
                }
                ctx.push_str("\n请严格遵循以上命名风格。\n\n");
                ctx
            } else {
                String::new()
            };

            // 构建带历史上下文的新 prompt
            let new_prompt = format!("{}{}", history_context, prompt);

            // 用新会话重试（session = None 会创建新会话）
            info!("[{}] 使用新会话重试，带上 {} 条历史记录", source_key, history.len());
            super::deepseek_web::deepseek_web_generate_raw(
                &token,
                None, // 创建新会话
                &new_prompt,
                cfg.timeout_seconds,
            )
            .await?
        }
        Err(e)
            if e.to_string().contains("读取响应体失败") || e.to_string().contains("error decoding response body") =>
        {
            warn!("[{}] DeepSeek 响应体解码失败，正在重建会话并带上历史...", source_key);

            // 清除旧会话缓存
            {
                let mut cache = DEEPSEEK_SESSION_CACHE.lock().await;
                cache.remove(source_key);
            }

            // 获取历史记录作为上下文
            let history = get_conversation_history(db.as_ref(), source_key).await;
            let history_context = if !history.is_empty() {
                let mut ctx = String::from("【之前的命名风格参考】\n");
                for msg in &history {
                    if msg.role == "assistant" {
                        ctx.push_str(&format!("{}\n", msg.content));
                    }
                }
                ctx.push_str("\n请严格遵循以上命名风格。\n\n");
                ctx
            } else {
                String::new()
            };

            // 构建带历史上下文的新 prompt
            let new_prompt = format!("{}{}", history_context, prompt);

            // 用新会话重试
            info!(
                "[{}] 使用新会话重试（响应体错误），带上 {} 条历史记录",
                source_key,
                history.len()
            );
            super::deepseek_web::deepseek_web_generate_raw(&token, None, &new_prompt, cfg.timeout_seconds).await?
        }
        Err(e) => return Err(e),
    };

    // 更新会话缓存
    {
        let mut cache = DEEPSEEK_SESSION_CACHE.lock().await;
        cache.insert(source_key.to_string(), new_session.clone());
    }
    if let Err(e) = save_deepseek_session(db.as_ref(), source_key, &new_session).await {
        warn!("保存 DeepSeek 会话到数据库失败: {}", e);
    }

    // 保存简化的对话历史（供一致性检查参考命名风格）
    let simplified_user_msg = format!("为{}个文件生成命名", expected_count);
    if let Err(e) = add_conversation_message(db.as_ref(), source_key, "user", &simplified_user_msg).await {
        warn!("保存用户消息失败: {}", e);
    }
    // 解析响应并保存文件名列表
    let parsed_names = parse_batch_response(&response, expected_count);
    if let Ok(ref names) = parsed_names {
        let simplified_response = names.join("\n");
        if let Err(e) = add_conversation_message(db.as_ref(), source_key, "assistant", &simplified_response).await {
            warn!("保存助手回复失败: {}", e);
        }
    }

    parsed_names
}

/// OpenAI 兼容 API 批量生成
async fn ai_generate_filenames_batch_openai(
    cfg: &AiRenameConfig,
    source_key: &str,
    prompt: &str,
    expected_count: usize,
) -> Result<Vec<String>> {
    let api_key = cfg.api_key.clone().ok_or_else(|| anyhow!("API key missing"))?;

    let db = crate::database::get_global_db().ok_or_else(|| anyhow!("数据库连接不可用"))?;

    let history = get_conversation_history(db.as_ref(), source_key).await;

    let system_prompt = if history.is_empty() {
        "你是一个文件命名助手。返回 JSON 数组格式的文件名列表，不要解释。".to_string()
    } else {
        "你是一个文件命名助手。严格遵循之前的命名风格，返回 JSON 数组格式。".to_string()
    };

    let mut messages = Vec::with_capacity(2 + history.len());
    messages.push(ChatMessage {
        role: "system".to_string(),
        content: system_prompt,
    });
    for msg in &history {
        messages.push(ChatMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: prompt.to_string(),
    });

    let req_body = ChatRequest {
        model: cfg.model.clone(),
        messages,
        max_tokens: Some(512), // 批量需要更多 token
        temperature: Some(0.1),
        thinking: deepseek_thinking_override(cfg),
    };

    let client = Client::builder()
        .timeout(Duration::from_secs(cfg.timeout_seconds.max(30)))
        .build()?;

    let base = cfg.base_url.trim_end_matches('/');
    let res = client
        .post(format!("{}/chat/completions", base))
        .bearer_auth(api_key)
        .json(&req_body)
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(anyhow!("批量 AI 请求失败: {} {}", status, body));
    }

    let resp: ChatResponse = res.json().await?;
    let raw = extract_chat_content(&resp)?;

    // 保存简化的对话历史（只保存生成的文件名列表，作为命名风格参考）
    // 不保存完整的 prompt（太长），只保存一个简短的用户消息和生成的文件名
    let simplified_user_msg = format!("为{}个文件生成命名", expected_count);
    if let Err(e) = add_conversation_message(db.as_ref(), source_key, "user", &simplified_user_msg).await {
        warn!("保存用户消息失败: {}", e);
    }
    let parsed_names = parse_batch_response(&raw, expected_count);
    let simplified_response = parsed_names
        .as_ref()
        .map(|names| names.join("\n"))
        .unwrap_or_else(|_| raw.clone());
    if let Err(e) = add_conversation_message(db.as_ref(), source_key, "assistant", &simplified_response).await {
        warn!("保存助手回复失败: {}", e);
    }

    parsed_names
}

fn deepseek_thinking_override(cfg: &AiRenameConfig) -> Option<ThinkingConfig> {
    let provider = cfg.provider.as_str();
    let base_url = cfg.base_url.to_ascii_lowercase();
    if provider == "deepseek" || base_url.contains("api.deepseek.com") {
        Some(ThinkingConfig {
            mode: "disabled".to_string(),
        })
    } else {
        None
    }
}

fn extract_chat_content(resp: &ChatResponse) -> Result<String> {
    let choice = resp.choices.first().ok_or_else(|| anyhow!("AI 响应缺少 choices"))?;
    let content = choice.message.content.as_deref().unwrap_or("").trim();
    if !content.is_empty() {
        return Ok(content.to_string());
    }

    let finish_reason = choice.finish_reason.as_deref().unwrap_or("unknown");
    let reasoning_len = choice
        .message
        .reasoning_content
        .as_deref()
        .map(|content| content.chars().count())
        .unwrap_or(0);
    if reasoning_len > 0 {
        return Err(anyhow!(
            "AI 返回最终内容为空: finish_reason={}, reasoning_content_len={}；模型只返回了推理内容，没有返回文件名 JSON 数组",
            finish_reason,
            reasoning_len
        ));
    }

    Err(anyhow!(
        "AI 返回最终内容为空: finish_reason={}；请重试或检查模型是否可用",
        finish_reason
    ))
}

fn normalize_ai_filename_punctuation(input: &str) -> String {
    let mut output = input.replace("……", "...");
    output = output.replace('…', "...");

    let mut normalized = String::with_capacity(output.len());
    for ch in output.chars() {
        match ch {
            '【' | '〖' | '〔' | '「' | '『' | '《' | '〈' => normalized.push('['),
            '】' | '〗' | '〕' | '」' | '』' | '》' | '〉' => normalized.push(']'),
            '（' => normalized.push('('),
            '）' => normalized.push(')'),
            '，' | '、' => normalized.push(','),
            '。' => normalized.push('.'),
            '；' => normalized.push(';'),
            '！' => normalized.push('!'),
            '：' | ':' | '？' | '?' => normalized.push('-'),
            '“' | '”' | '‘' | '’' | '"' => {}
            '\u{3000}' => normalized.push(' '),
            _ => normalized.push(ch),
        }
    }

    remove_spaces_after_closing_bracket(&normalized)
}

fn remove_spaces_after_closing_bracket(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        output.push(ch);
        if ch == ']' {
            while matches!(chars.peek(), Some(' ' | '\t' | '\u{3000}')) {
                chars.next();
            }
        }
    }

    output
}

fn cleanup_ai_filename_separators(input: &str) -> String {
    let mut output = input.trim().to_string();
    while output.contains("--") {
        output = output.replace("--", "-");
    }
    while output.contains(",,") {
        output = output.replace(",,", ",");
    }

    output
        .trim_matches(|ch| matches!(ch, '-' | ',' | ';' | ' ' | '\t' | '\u{3000}'))
        .trim_end_matches('.')
        .to_string()
}

/// 解析批量响应的 JSON 数组
fn parse_batch_response(response: &str, expected_count: usize) -> Result<Vec<String>> {
    let response = response.trim();
    // 尝试提取 JSON 数组
    let json_str = if let Some(start) = response.find('[') {
        if let Some(end) = response.rfind(']') {
            &response[start..=end]
        } else {
            response
        }
    } else {
        response
    };

    // 解析 JSON
    let names: Vec<String> =
        serde_json::from_str(json_str).map_err(|e| anyhow!("解析 JSON 数组失败: {} - 原始响应: {}", e, response))?;

    if names.len() != expected_count {
        warn!("AI 返回数量不匹配: 期望 {}, 实际 {}", expected_count, names.len());
    }

    // 清洗文件名
    let cleaned: Vec<String> = names
        .into_iter()
        .map(|name| {
            let mut n = name.replace(['"', '\n', '\r'], "");
            n = normalize_ai_filename_punctuation(&n);
            n = n.replace(' ', "-");
            n = cleanup_ai_filename_separators(&n);
            n = crate::utils::filenamify::filenamify(&n);
            n = cleanup_ai_filename_separators(&n);
            if n.chars().count() > 180 {
                n = n.chars().take(180).collect();
            }
            n
        })
        .collect();

    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::parse_batch_response;
    use super::{
        deepseek_thinking_override, extract_chat_content, AiRenameConfig, AiRenameContext, ChatMessageResponse,
        ChatResponse, Choice, FileToRename,
    };

    #[test]
    fn ai_context_json_marks_multi_page_and_bangumi() {
        let ctx = AiRenameContext {
            title: "测试视频".to_string(),
            owner: "测试UP".to_string(),
            source_type: "投稿".to_string(),
            is_multi_page: true,
            is_bangumi: false,
            structure: "Season 结构".to_string(),
            pid: 2,
            part_name: "P2 内容".to_string(),
            ..Default::default()
        };
        let json = ctx.to_json_string();
        assert!(json.contains("多P视频"), "应标记多P类型: {json}");
        assert!(json.contains("Season 结构"), "应标记目录结构: {json}");
        assert!(json.contains("P2"), "应包含分P信息: {json}");

        let bangumi = AiRenameContext {
            title: "番剧视频".to_string(),
            owner: "番剧".to_string(),
            source_type: "番剧".to_string(),
            is_bangumi: true,
            structure: "番剧Season结构".to_string(),
            ..Default::default()
        };
        let json = bangumi.to_json_string();
        assert!(json.contains("番剧"), "应标记番剧类型: {json}");
        assert!(json.contains("番剧Season结构"), "应标记番剧结构: {json}");

        let single = AiRenameContext {
            title: "单P".to_string(),
            owner: "UP".to_string(),
            source_type: "投稿".to_string(),
            ..Default::default()
        };
        let json = single.to_json_string();
        assert!(json.contains("单P视频"), "应标记单P类型: {json}");
    }

    #[test]
    fn batch_filenames_prompt_enforces_one_to_one_and_marks_types() {
        let file1 = FileToRename {
            path: std::path::PathBuf::from("P01.mp4"),
            current_stem: "P01".to_string(),
            ext: "mp4".to_string(),
            ctx: AiRenameContext {
                title: "多P测试".to_string(),
                owner: "UP".to_string(),
                source_type: "投稿".to_string(),
                is_multi_page: true,
                structure: "Season 结构".to_string(),
                pid: 1,
                part_name: "P1 开场".to_string(),
                ..Default::default()
            },
            page_id: 1,
            video_id: 1,
            bvid: "BV1".to_string(),
            single_page: false,
            flat_folder: false,
        };
        let file2 = FileToRename {
            path: std::path::PathBuf::from("P02.mp4"),
            current_stem: "P02".to_string(),
            ext: "mp4".to_string(),
            ctx: AiRenameContext {
                title: "多P测试".to_string(),
                owner: "UP".to_string(),
                source_type: "投稿".to_string(),
                is_multi_page: true,
                structure: "Season 结构".to_string(),
                pid: 2,
                part_name: "P2 内容".to_string(),
                ..Default::default()
            },
            page_id: 2,
            video_id: 1,
            bvid: "BV1".to_string(),
            single_page: false,
            flat_folder: false,
        };
        let prompt = super::build_batch_filenames_prompt(&[file1, file2], "作者-标题-序号");
        assert!(prompt.contains("一一对应"), "应包含一一对应约束");
        assert!(prompt.contains("数量必须与文件数量完全一致"), "应包含数量一致约束");
        assert!(prompt.contains("多P视频"), "条目应标记多P类型");
        assert!(prompt.contains("Season 结构"), "条目应标记结构");
        assert!(prompt.contains("1. 当前文件名: P01"), "应包含文件1条目");
        assert!(prompt.contains("2. 当前文件名: P02"), "应包含文件2条目");
    }

    #[test]
    fn parse_batch_response_accepts_json_array() {
        let names = parse_batch_response(r#"["ZHY20202024-06-09", "三国bigbig2024-06-09"]"#, 2)
            .expect("valid json array should parse");

        assert_eq!(names, vec!["ZHY20202024-06-09", "三国bigbig2024-06-09"]);
    }

    #[test]
    fn parse_batch_response_normalizes_ai_punctuation() {
        let names = parse_batch_response(
            r#"[
                "【枪神纪】 大楼屋顶-榴弹点位荧块",
                "[崩坏:星穹铁道]罗浮仙舟-镜流剧情?",
                "[音乐]歌名（现场版）-",
                "[电影解说]“开局即巅峰”-这部片太重磅了..."
            ]"#,
            4,
        )
        .expect("valid json array should parse");

        assert_eq!(
            names,
            vec![
                "[枪神纪]大楼屋顶-榴弹点位荧块",
                "[崩坏-星穹铁道]罗浮仙舟-镜流剧情",
                "[音乐]歌名(现场版)",
                "[电影解说]开局即巅峰-这部片太重磅了"
            ]
        );
    }

    #[test]
    fn resolves_multi_page_video_folder() {
        use std::path::PathBuf;

        // 无 Season 结构：所有分页文件在同一目录
        let p1 = PathBuf::from("F:/下载/视频A/P01.mp4");
        let p2 = PathBuf::from("F:/下载/视频A/P02.mp4");
        let parents = vec![
            p1.parent().unwrap().to_path_buf(),
            p2.parent().unwrap().to_path_buf(),
        ];
        assert_eq!(
            super::resolve_multi_page_video_folder(&parents),
            Some(PathBuf::from("F:/下载/视频A"))
        );

        // Season 结构：分页文件位于「视频文件夹/Season XX」下
        let p3 = PathBuf::from("F:/下载/视频B/Season 01/P01.mp4");
        let p4 = PathBuf::from("F:/下载/视频B/Season 01/P02.mp4");
        let parents = vec![
            p3.parent().unwrap().to_path_buf(),
            p4.parent().unwrap().to_path_buf(),
        ];
        assert_eq!(
            super::resolve_multi_page_video_folder(&parents),
            Some(PathBuf::from("F:/下载/视频B"))
        );

        // 同一视频跨季（结构异常但仍在同一视频文件夹下）：仍能反推出视频文件夹
        let p5 = PathBuf::from("F:/下载/视频B/Season 02/P01.mp4");
        let parents = vec![
            p3.parent().unwrap().to_path_buf(),
            p5.parent().unwrap().to_path_buf(),
        ];
        assert_eq!(
            super::resolve_multi_page_video_folder(&parents),
            Some(PathBuf::from("F:/下载/视频B"))
        );

        // 分页散落在不同视频文件夹下：无法确定单一文件夹
        let p6 = PathBuf::from("F:/下载/视频C/Season 01/P01.mp4");
        let parents = vec![
            p3.parent().unwrap().to_path_buf(),
            p6.parent().unwrap().to_path_buf(),
        ];
        assert_eq!(super::resolve_multi_page_video_folder(&parents), None);

        // 空输入
        assert_eq!(super::resolve_multi_page_video_folder(&[]), None);
    }

    #[test]
    fn parse_batch_response_rejects_missing_array_prefix() {
        let err = parse_batch_response(r#"ZHY20202024-06-09", "三国bigbig2024-06-09"]"#, 2)
            .expect_err("truncated json array should stay invalid");

        assert!(err.to_string().contains("解析 JSON 数组失败"));
    }

    #[test]
    fn deepseek_paid_api_disables_thinking_for_filename_tasks() {
        let cfg = AiRenameConfig {
            enabled: true,
            provider: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            ..Default::default()
        };

        let thinking = deepseek_thinking_override(&cfg).expect("deepseek provider should disable thinking");
        assert_eq!(thinking.mode, "disabled");
    }

    #[test]
    fn custom_provider_does_not_send_deepseek_thinking_field() {
        let cfg = AiRenameConfig {
            enabled: true,
            provider: "custom".to_string(),
            base_url: "https://example.com/v1".to_string(),
            ..Default::default()
        };

        assert!(deepseek_thinking_override(&cfg).is_none());
    }

    #[test]
    fn extract_chat_content_reports_reasoning_only_response() {
        let resp = ChatResponse {
            choices: vec![Choice {
                message: ChatMessageResponse {
                    content: Some(String::new()),
                    reasoning_content: Some("thinking".to_string()),
                },
                finish_reason: Some("length".to_string()),
            }],
        };

        let err = extract_chat_content(&resp).expect_err("empty final content should fail clearly");
        let message = err.to_string();
        assert!(message.contains("最终内容为空"));
        assert!(message.contains("reasoning_content_len=8"));
        assert!(message.contains("finish_reason=length"));
    }
}

/// 收集多P视频的独占文件夹（用于 AI 重命名整个视频文件夹）
///
/// 判定规则：
/// - 视频必须是多P（分页数 > 1），且不在平铺目录模式下；
/// - 所有分页文件位于同一个视频文件夹下（无 Season 结构），或位于
///   「视频文件夹/Season XX」结构下（有 Season 结构）；
/// - 该文件夹没有被其他视频共享（其他 video 的 path 也指向同一目录时跳过，
///   例如 UP 分组目录 / 合集统一目录 / 收藏夹按 UP 分组目录）。
///
/// `skip_if_all_pages_renamed` 为 true 时，若该视频的所有分页都已被 AI
/// 重命名过（page.ai_renamed = 1），则不再收集文件夹，避免重复调用 AI。
pub async fn collect_multi_page_folders(
    connection: &DatabaseConnection,
    videos: &[(bili_sync_entity::video::Model, Vec<bili_sync_entity::page::Model>)],
    flat_folder: bool,
    skip_if_all_pages_renamed: bool,
) -> Vec<FolderToRename> {
    use bili_sync_entity::video;

    if flat_folder {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (video_model, pages) in videos {
        if pages.len() <= 1 {
            continue;
        }
        if skip_if_all_pages_renamed && pages.iter().all(|p| p.ai_renamed.unwrap_or(0) == 1) {
            debug!("[{}] 跳过多P文件夹重命名（所有分页均已 AI 重命名）: {}", video_model.id, video_model.name);
            continue;
        }
        // 收集有真实路径的分页
        let paths: Vec<std::path::PathBuf> = pages
            .iter()
            .filter_map(|p| {
                p.path
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(std::path::PathBuf::from)
            })
            .collect();
        if paths.len() < 2 {
            continue;
        }
        // 找到分页文件所在目录
        let parents: Vec<std::path::PathBuf> = paths
            .iter()
            .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
            .collect();
        if parents.is_empty() {
            continue;
        }
        // 推导视频文件夹：所有分页同目录（无 Season 结构），
        // 或位于「视频文件夹/Season XX」结构下
        let Some(video_folder) = resolve_multi_page_video_folder(&parents) else {
            debug!(
                "[{}] 跳过多P文件夹重命名（分页目录结构不规整）: {}",
                video_model.id, video_model.name
            );
            continue;
        };
        // 文件夹必须存在且是目录
        if !video_folder.is_dir() {
            continue;
        }
        // 独占判定：没有其他 video 共享该目录
        let folder_str = video_folder.to_string_lossy().to_string();
        let folder_str_alt = folder_str.replace('/', "\\");
        let shared_count = match video::Entity::find()
            .filter(video::Column::Id.ne(video_model.id))
            .filter(video::Column::Path.eq(&folder_str).or(video::Column::Path.eq(&folder_str_alt)))
            .count(connection)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                warn!("查询共享文件夹失败 video_id={}: {}", video_model.id, e);
                0
            }
        };
        if shared_count > 0 {
            debug!(
                "[{}] 跳过多P文件夹重命名（其他视频也使用该目录）: {}",
                video_model.id, folder_str
            );
            continue;
        }
        let current_name = video_folder
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if current_name.is_empty() {
            continue;
        }
        let ctx = AiRenameContext {
            title: video_model.name.clone(),
            desc: video_model.intro.clone(),
            owner: video_model.upper_name.clone(),
            tname: String::new(),
            duration: 0,
            pubdate: video_model.pubtime.format("%Y%m%d%H%M%S").to_string(),
            dimension: String::new(),
            part_name: String::new(),
            ugc_season: None,
            copyright: String::new(),
            view: 0,
            pid: pages.len() as i32,
            episode_number: None,
            source_type: String::new(),
            is_audio: false,
            sort_index: None,
            bvid: video_model.bvid.clone(),
            is_multi_page: true,
            is_bangumi: false,
            structure: "多P视频文件夹".to_string(),
        };
        out.push(FolderToRename {
            path: video_folder,
            current_name,
            ctx,
            video_id: video_model.id,
            bvid: video_model.bvid.clone(),
        });
    }
    out
}

/// 从分页文件目录列表推导多P视频的文件夹路径。
///
/// - 所有分页文件位于同一目录（无 Season 结构）→ 直接返回该目录；
/// - 分页文件位于「视频文件夹/Season XX」结构下 → 返回 Season 的上一级；
/// - 其它不规整结构 → None。
fn resolve_multi_page_video_folder(parents: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    if parents.is_empty() {
        return None;
    }
    // 所有分页文件位于同一目录
    if parents.iter().all(|d| d == &parents[0]) {
        let is_season_dir = parents[0]
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.to_lowercase().starts_with("season "));
        // 目录本身是「Season XX」时说明处于 Season 结构，视频文件夹是上一级；
        // 否则该目录就是视频文件夹（无 Season 结构）。
        if is_season_dir {
            return parents[0].parent().map(|g| g.to_path_buf());
        }
        return Some(parents[0].clone());
    }
    resolve_season_parent(parents)
}

/// 多P视频处于「视频文件夹/Season XX」结构时，从分页文件目录反推视频文件夹。
fn resolve_season_parent(parents: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    let grandparents: Vec<Option<std::path::PathBuf>> = parents
        .iter()
        .map(|d| d.parent().map(|g| g.to_path_buf()))
        .collect();
    let first = grandparents.first()?.as_ref()?;
    if !grandparents.iter().all(|g| g.as_ref() == Some(first)) {
        return None;
    }
    if !parents.iter().all(|d| {
        d.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.to_lowercase().starts_with("season "))
    }) {
        return None;
    }
    Some(first.clone())
}

/// 应用单个视频文件夹的 AI 重命名（多P视频独占文件夹）
///
/// 重命名文件夹后同步更新数据库中的 video.path 与该视频所有分页的 page.path，
/// 保证后续扫描/播放/重设路径等逻辑仍然能找到文件。
///
/// 返回 Ok(true) 表示成功重命名，Ok(false) 表示跳过。
pub async fn apply_ai_rename_folder(
    connection: &DatabaseConnection,
    source_key: &str,
    folder: &FolderToRename,
    new_name: &str,
) -> Result<bool> {
    use bili_sync_entity::{page, video};

    let new_name = new_name.trim();
    if new_name.is_empty() || new_name == folder.current_name {
        debug!(
            "[{}] 跳过文件夹重命名(相同或为空): {}",
            source_key, folder.current_name
        );
        return Ok(false);
    }
    // 清理非法字符，防止 AI 生成路径分隔符或非法文件名
    let safe_name = crate::utils::filenamify::filenamify(new_name);
    let safe_name = safe_name.trim().to_string();
    if safe_name.is_empty() || safe_name == folder.current_name {
        debug!(
            "[{}] 跳过文件夹重命名(清理后相同或为空): {}",
            source_key, folder.current_name
        );
        return Ok(false);
    }
    let old_dir = &folder.path;
    // 重新确认文件夹仍存在
    if !old_dir.is_dir() {
        debug!(
            "[{}] 跳过文件夹重命名(目录不存在): {}",
            source_key,
            old_dir.display()
        );
        return Ok(false);
    }
    let parent_dir = match old_dir.parent() {
        Some(d) => d,
        None => return Ok(false),
    };
    let mut target_dir = parent_dir.join(&safe_name);
    if target_dir.exists() && target_dir != *old_dir {
        let bvid = folder.bvid.trim();
        target_dir = parent_dir.join(if bvid.is_empty() {
            safe_name.clone()
        } else {
            format!("{}-{}", safe_name, bvid)
        });
        info!(
            "[{}] 检测到文件夹名冲突，追加BV号: {} -> {}",
            source_key,
            safe_name,
            target_dir.file_name().and_then(|s| s.to_str()).unwrap_or("")
        );
    }
    if target_dir == *old_dir {
        return Ok(false);
    }
    if target_dir.exists() {
        info!(
            "[{}] 跳过文件夹重命名(目标目录已存在): {}",
            source_key,
            target_dir.display()
        );
        return Ok(false);
    }
    if let Err(e) = fs::rename(old_dir, &target_dir) {
        warn!(
            "[{}] 重命名文件夹失败: {} -> {}: {}",
            source_key,
            old_dir.display(),
            target_dir.display(),
            e
        );
        return Ok(false);
    }
    info!(
        "[{}] AI 重命名文件夹成功: {} -> {}",
        source_key,
        old_dir.display(),
        target_dir.display()
    );

    let old_dir_str = old_dir.to_string_lossy().to_string();
    let old_dir_str_alt = old_dir_str.replace('/', "\\");
    let new_dir_str = target_dir.to_string_lossy().to_string();

    // 更新 video.path
    if let Ok(Some(current_video)) = video::Entity::find_by_id(folder.video_id).one(connection).await {
        let mut active_video: video::ActiveModel = current_video.into();
        active_video.path = Set(new_dir_str.clone());
        if let Err(e) = active_video.update(connection).await {
            warn!("[{}] 更新 video.path 失败: {}", source_key, e);
        }
    }
    // 更新该视频所有分页的 page.path
    if let Ok(pages) = page::Entity::find()
        .filter(page::Column::VideoId.eq(folder.video_id))
        .all(connection)
        .await
    {
        for page_model in pages {
            if let Some(page_path_str) = page_model.path.clone() {
                if page_path_str.starts_with(&old_dir_str) || page_path_str.starts_with(&old_dir_str_alt) {
                    let new_page_path = if page_path_str.starts_with(&old_dir_str) {
                        page_path_str.replacen(&old_dir_str, &new_dir_str, 1)
                    } else {
                        page_path_str.replacen(&old_dir_str_alt, &new_dir_str, 1)
                    };
                    let mut active_page: page::ActiveModel = page_model.into();
                    active_page.path = Set(Some(new_page_path.clone()));
                    if let Err(e) = active_page.update(connection).await {
                        warn!("[{}] 更新 page.path 失败: {}", source_key, e);
                    } else {
                        info!(
                            "[{}] 同步更新分页路径: {} -> {}",
                            source_key, page_path_str, new_page_path
                        );
                    }
                }
            }
        }
    }
    Ok(true)
}

/// 批量重命名视频源下的历史文件
///
/// # 参数
/// - `connection`: 数据库连接
/// - `source_key`: 视频源唯一标识（如 "collection_123"）
/// - `videos`: 视频和其分页列表（已下载的）
/// - `config`: AI 重命名配置
/// - `video_prompt`: 视频自定义提示词
/// - `audio_prompt`: 音频自定义提示词
/// - `flat_folder`: 是否为平铺目录模式
///
/// # 返回
/// - 批量重命名结果（renamed_count, skipped_count, failed_count）
pub async fn batch_rename_history_files(
    connection: &DatabaseConnection,
    source_key: &str,
    videos: Vec<(bili_sync_entity::video::Model, Vec<bili_sync_entity::page::Model>)>,
    config: &AiRenameConfig,
    video_prompt: &str,
    audio_prompt: &str,
    flat_folder: bool,
    structure_hint: &str,
) -> Result<BatchRenameResult> {
    let mut result = BatchRenameResult::default();

    // 第一步：收集所有需要重命名的文件
    let mut video_files: Vec<FileToRename> = Vec::new();
    let mut audio_files: Vec<FileToRename> = Vec::new();

    info!("[{}] 开始批量重命名，共 {} 个视频", source_key, videos.len());

    // 跟踪视频和音频的排序位置
    let mut video_sort_index = 1;
    let mut audio_sort_index = 1;

    for (video, pages) in &videos {
        // 仅当该视频只有 1 个分页时，才允许后续进行“子文件夹重命名”
        //（多P视频文件夹内通常有多段文件，重命名文件夹容易导致路径错乱）
        let is_single_page_video = pages.len() == 1;
        for page_model in pages {
            // 检查 page.path 是否存在
            let page_path = match &page_model.path {
                Some(p) if !p.is_empty() => p.clone(),
                _ => {
                    debug!("[{}] 跳过 page_id={}: path 为空", source_key, page_model.id);
                    result.skipped_count += 1;
                    continue;
                }
            };

            // 检查文件是否存在
            let file_path = Path::new(&page_path);
            if !file_path.exists() {
                debug!(
                    "[{}] 跳过 page_id={}: 文件不存在 path={}",
                    source_key, page_model.id, page_path
                );
                result.skipped_count += 1;
                continue;
            }

            // 获取当前文件名和扩展名
            let current_stem = match file_path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => {
                    result.failed_count += 1;
                    continue;
                }
            };

            let ext = file_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("mp4")
                .to_string();

            let is_audio = matches!(ext.as_str(), "m4a" | "mp3" | "flac" | "aac" | "ogg");

            // 根据文件类型获取排序位置
            let current_sort_index = if is_audio {
                let idx = audio_sort_index;
                audio_sort_index += 1;
                idx
            } else {
                let idx = video_sort_index;
                video_sort_index += 1;
                idx
            };

            let ctx = AiRenameContext {
                title: video.name.clone(),
                desc: video.intro.clone(),
                owner: video.upper_name.clone(),
                tname: video.category.to_string(),
                duration: 0,
                pubdate: video.pubtime.format("%Y%m%d%H%M%S").to_string(),
                dimension: String::new(),
                part_name: page_model.name.clone(),
                ugc_season: None,
                copyright: String::new(),
                view: 0,
                pid: page_model.pid,
                episode_number: None,
                source_type: source_key.split('_').next().unwrap_or("unknown").to_string(),
                is_audio,
                sort_index: Some(current_sort_index),
                bvid: video.bvid.clone(),
                is_multi_page: !is_single_page_video,
                is_bangumi: source_key.starts_with("bangumi"),
                structure: structure_hint.to_string(),
            };

            let file_info = FileToRename {
                path: file_path.to_path_buf(),
                current_stem,
                ext,
                ctx,
                page_id: page_model.id,
                video_id: video.id,
                bvid: video.bvid.clone(),
                single_page: is_single_page_video,
                flat_folder,
            };

            if is_audio {
                audio_files.push(file_info);
            } else {
                video_files.push(file_info);
            }
        }
    }

    info!(
        "[{}] 收集完成: {} 个视频文件, {} 个音频文件",
        source_key,
        video_files.len(),
        audio_files.len()
    );

    // 收集多P视频独占文件夹（整个视频文件夹的 AI 重命名）
    let folder_renames = collect_multi_page_folders(connection, &videos, flat_folder, false).await;
    if !folder_renames.is_empty() {
        info!(
            "[{}] 收集到 {} 个多P视频文件夹待重命名",
            source_key,
            folder_renames.len()
        );
    }

    // 第二步：按批次处理视频文件（每批 10 个）
    let batch_size = 10;
    let video_prompt_hint = if !video_prompt.is_empty() {
        video_prompt
    } else {
        &config.video_prompt_hint
    };
    let audio_prompt_hint = if !audio_prompt.is_empty() {
        audio_prompt
    } else {
        &config.audio_prompt_hint
    };

    // 处理视频文件
    for (batch_idx, batch) in video_files.chunks(batch_size).enumerate() {
        info!(
            "[{}] 处理视频批次 {}/{}: {} 个文件",
            source_key,
            batch_idx + 1,
            (video_files.len() + batch_size - 1) / batch_size,
            batch.len()
        );

        match ai_generate_filenames_batch(config, source_key, batch, video_prompt_hint).await {
            Ok(new_names) => {
                for (file, new_stem) in batch.iter().zip(new_names.iter()) {
                    apply_rename(connection, source_key, file, new_stem, config, &mut result).await;
                }
            }
            Err(e) => {
                warn!("[{}] 视频批次 {} 处理失败: {}", source_key, batch_idx + 1, e);
                result.failed_count += batch.len();
            }
        }
    }

    // 处理音频文件
    for (batch_idx, batch) in audio_files.chunks(batch_size).enumerate() {
        info!(
            "[{}] 处理音频批次 {}/{}: {} 个文件",
            source_key,
            batch_idx + 1,
            (audio_files.len() + batch_size - 1) / batch_size,
            batch.len()
        );

        match ai_generate_filenames_batch(config, source_key, batch, audio_prompt_hint).await {
            Ok(new_names) => {
                for (file, new_stem) in batch.iter().zip(new_names.iter()) {
                    apply_rename(connection, source_key, file, new_stem, config, &mut result).await;
                }
            }
            Err(e) => {
                warn!("[{}] 音频批次 {} 处理失败: {}", source_key, batch_idx + 1, e);
                result.failed_count += batch.len();
            }
        }
    }

    // 处理多P视频文件夹重命名（需开启「重命名上级目录」，与单P保持一致）
    if config.rename_parent_dir && !folder_renames.is_empty() {
        info!(
            "[{}] 开始处理 {} 个多P视频文件夹的重命名",
            source_key,
            folder_renames.len()
        );
        for (batch_idx, batch) in folder_renames.chunks(batch_size).enumerate() {
            info!(
                "[{}] 处理文件夹批次 {}/{}: {} 个文件夹",
                source_key,
                batch_idx + 1,
                (folder_renames.len() + batch_size - 1) / batch_size,
                batch.len()
            );
            match ai_generate_folder_names_batch(config, source_key, batch, video_prompt_hint).await {
                Ok(new_names) => {
                    for (folder, new_name) in batch.iter().zip(new_names.iter()) {
                        match apply_ai_rename_folder(connection, source_key, folder, new_name).await {
                            Ok(true) => result.renamed_count += 1,
                            Ok(false) => result.skipped_count += 1,
                            Err(e) => {
                                warn!(
                                    "[{}] 文件夹重命名失败 {}: {}",
                                    source_key, folder.current_name, e
                                );
                                result.failed_count += 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "[{}] 文件夹批次 {} AI 调用失败: {}",
                        source_key,
                        batch_idx + 1,
                        e
                    );
                    result.failed_count += batch.len();
                }
            }
        }
    }

    info!(
        "[{}] 批量重命名完成: 重命名 {} 个, 跳过 {} 个, 失败 {} 个",
        source_key, result.renamed_count, result.skipped_count, result.failed_count
    );

    Ok(result)
}

/// 应用单个文件的重命名
async fn apply_rename(
    connection: &DatabaseConnection,
    source_key: &str,
    file: &FileToRename,
    new_stem: &str,
    config: &AiRenameConfig,
    result: &mut BatchRenameResult,
) {
    use bili_sync_entity::{page, video};

    // 文件名相同则跳过
    if new_stem == file.current_stem {
        info!("[{}] 跳过(文件名相同): '{}'", source_key, new_stem);
        result.skipped_count += 1;
        return;
    }

    // 新文件名为空则跳过
    if new_stem.is_empty() {
        info!("[{}] 跳过(AI返回空): 原文件名 '{}'", source_key, file.current_stem);
        result.skipped_count += 1;
        return;
    }

    // 构建新路径（处理重复文件名）
    let parent = file.path.parent().unwrap_or(Path::new("."));
    let mut final_stem = new_stem.to_string();
    let mut new_filename = format!("{}.{}", final_stem, file.ext);
    let mut new_path = parent.join(&new_filename);

    // 如果目标文件已存在，添加后缀使其唯一
    let mut suffix = 1;
    while new_path.exists() {
        final_stem = format!("{}-{}", new_stem, suffix);
        new_filename = format!("{}.{}", final_stem, file.ext);
        new_path = parent.join(&new_filename);
        suffix += 1;
        if suffix > 99 {
            // 防止无限循环
            info!(
                "[{}] 跳过(无法生成唯一文件名): {} -> {}",
                source_key, file.current_stem, new_stem
            );
            result.skipped_count += 1;
            return;
        }
    }

    // 如果添加了后缀，记录日志
    if suffix > 1 {
        info!(
            "[{}] 检测到重复文件名，自动添加后缀: {} -> {}",
            source_key, new_stem, final_stem
        );
    }

    // 执行文件重命名
    if let Err(e) = fs::rename(&file.path, &new_path) {
        warn!(
            "[{}] 重命名文件失败: {} -> {} - {}",
            source_key,
            file.path.display(),
            new_path.display(),
            e
        );
        result.failed_count += 1;
        return;
    }

    info!("[{}] 重命名成功: {} -> {}", source_key, file.current_stem, final_stem);

    // 重命名侧车文件
    if let Err(e) = rename_sidecars(&file.path, &final_stem, &file.ext) {
        warn!("[{}] 重命名侧车文件失败: {}", source_key, e);
    }

    // 更新NFO文件内容（标题标签）
    let nfo_path = parent.join(format!("{}.nfo", final_stem));
    if let Err(e) = update_nfo_content(&nfo_path, &final_stem) {
        warn!("[{}] 更新NFO内容失败: {}", source_key, e);
    }

    // 重命名子文件夹（仅单P视频 且 非平铺目录模式）
    // 合集/多P 场景下重命名文件夹容易导致路径错乱，因此这里直接跳过。
    let should_rename_folder =
        config.rename_parent_dir && !file.flat_folder && file.single_page && file.ctx.source_type != "collection";

    let final_path = if should_rename_folder {
        if let Some(old_dir) = new_path.parent() {
            if let Some(parent_dir) = old_dir.parent() {
                let mut target_dir = parent_dir.join(&final_stem);
                // 如果目标目录已存在且不是当前目录，追加 bvid 避免冲突
                if target_dir.exists() && target_dir != old_dir {
                    target_dir = parent_dir.join(format!("{}-{}", &final_stem, file.bvid));
                }

                if target_dir != old_dir {
                    match std::fs::rename(old_dir, &target_dir) {
                        Ok(_) => {
                            let moved_path =
                                target_dir.join(new_path.file_name().expect("new_path should have file name"));
                            info!(
                                "[{}] AI 重命名子文件夹成功: {} -> {}",
                                source_key,
                                old_dir.display(),
                                target_dir.display()
                            );

                            // 更新当前 video.path
                            let new_video_path = target_dir.to_string_lossy().to_string();
                            if let Ok(Some(current_video)) =
                                video::Entity::find_by_id(file.video_id).one(connection).await
                            {
                                let mut active_video: video::ActiveModel = current_video.into();
                                active_video.path = Set(new_video_path.clone());
                                if let Err(e) = active_video.update(connection).await {
                                    warn!("[{}] 更新 video.path 失败: {}", source_key, e);
                                }
                            }

                            // 更新同一文件夹中其他视频的路径
                            let old_dir_str = old_dir.to_string_lossy().to_string();
                            let old_dir_str_alt = old_dir_str.replace('/', "\\");

                            if let Ok(other_videos) = video::Entity::find()
                                .filter(video::Column::Id.ne(file.video_id))
                                .filter(
                                    video::Column::Path
                                        .eq(&old_dir_str)
                                        .or(video::Column::Path.eq(&old_dir_str_alt)),
                                )
                                .all(connection)
                                .await
                            {
                                for other_video in other_videos {
                                    let mut active_other: video::ActiveModel = other_video.clone().into();
                                    active_other.path = Set(new_video_path.clone());
                                    if let Err(e) = active_other.update(connection).await {
                                        warn!("[{}] 更新同文件夹其他视频 video.path 失败: {}", source_key, e);
                                    }

                                    // 更新其他视频的 page 路径
                                    if let Ok(other_pages) = page::Entity::find()
                                        .filter(page::Column::VideoId.eq(other_video.id))
                                        .all(connection)
                                        .await
                                    {
                                        for other_page in other_pages {
                                            if let Some(page_path_str) = other_page.path.clone() {
                                                if page_path_str.starts_with(&old_dir_str)
                                                    || page_path_str.starts_with(&old_dir_str_alt)
                                                {
                                                    let new_page_path = if page_path_str.starts_with(&old_dir_str) {
                                                        page_path_str.replacen(&old_dir_str, &new_video_path, 1)
                                                    } else {
                                                        page_path_str.replacen(&old_dir_str_alt, &new_video_path, 1)
                                                    };
                                                    let mut active_page: page::ActiveModel = other_page.into();
                                                    active_page.path = Set(Some(new_page_path.clone()));
                                                    if let Err(e) = active_page.update(connection).await {
                                                        warn!(
                                                            "[{}] 更新同文件夹其他视频 page.path 失败: {}",
                                                            source_key, e
                                                        );
                                                    } else {
                                                        info!(
                                                            "[{}] 同步更新同文件夹页面路径: {} -> {}",
                                                            source_key, page_path_str, new_page_path
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            moved_path
                        }
                        Err(e) => {
                            warn!("[{}] AI 重命名子文件夹失败: {}", source_key, e);
                            new_path.clone()
                        }
                    }
                } else {
                    new_path.clone()
                }
            } else {
                new_path.clone()
            }
        } else {
            new_path.clone()
        }
    } else {
        new_path.clone()
    };

    // 更新数据库中的 page.path 和 ai_renamed 标记
    let new_path_str = final_path.to_string_lossy().to_string();
    if let Ok(page_model) = page::Entity::find_by_id(file.page_id).one(connection).await {
        if let Some(page_model) = page_model {
            let mut active_page: page::ActiveModel = page_model.into();
            active_page.path = Set(Some(new_path_str));
            active_page.ai_renamed = Set(Some(1)); // 标记为已 AI 重命名，防止重复处理
            if let Err(e) = active_page.update(connection).await {
                warn!("[{}] 更新数据库 page.path 失败: {}", source_key, e);
            }
        }
    }

    result.renamed_count += 1;
}
