use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use crate::utils::live_updates;

/// 单条下载任务进度快照（推送 / API 使用）。
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct DownloadProgressItem {
    /// 任务唯一键（B 站：`bilibili:{video_id}:{page_id}`，外源：`youtube:{id}`）
    pub key: String,
    /// 平台：bilibili / youtube / douyin / tiktok
    pub platform: String,
    /// 视频标题
    pub title: String,
    /// 当前阶段：解析直链 / 下载视频流 / 下载音频流 / 合并中 / 下载中…
    pub phase: String,
    /// 目标文件名
    pub file_name: String,
    /// 已下载字节（0 表示总大小未知时无法计算百分比）
    pub downloaded_bytes: u64,
    /// 总字节（0 表示未知，如 yt-dlp 子进程下载）
    pub total_bytes: u64,
    /// 实时速度（字节/秒）
    pub speed_bps: u64,
    /// 预计剩余秒数
    pub eta_seconds: Option<u64>,
    /// 开始时间（`%Y-%m-%d %H:%M:%S`）
    pub started_at: String,
}

/// 一个流的下载状态（一个 tmp_video / tmp_audio / 最终文件）。
#[derive(Debug)]
struct StreamState {
    total_bytes: AtomicU64,
    downloaded_bytes: AtomicU64,
    /// (最近上报时间, 最近上报字节, 平滑速度)
    last_report: Mutex<(Instant, u64, u64)>,
}

impl StreamState {
    fn new() -> Self {
        Self {
            total_bytes: AtomicU64::new(0),
            downloaded_bytes: AtomicU64::new(0),
            last_report: Mutex::new((Instant::now(), 0, 0)),
        }
    }
}

/// 一个下载任务的元信息与流集合。
#[derive(Debug)]
struct TaskState {
    key: String,
    platform: String,
    title: String,
    phase: String,
    file_name: String,
    started_at: String,
    streams: HashMap<String, StreamState>,
}

impl TaskState {
    fn total_bytes(&self) -> u64 {
        self.streams.values().map(|s| s.total_bytes.load(Ordering::Relaxed)).sum()
    }

    fn downloaded_bytes(&self) -> u64 {
        self.streams.values().map(|s| s.downloaded_bytes.load(Ordering::Relaxed)).sum()
    }

}

pub struct DownloadProgress {
    /// key -> 任务
    tasks: RwLock<HashMap<String, TaskState>>,
    /// 文件路径 -> 任务 key（download_stream 下载时按路径关联任务）
    path_to_key: RwLock<HashMap<String, String>>,
    /// 进度推送节流（至少间隔这么久才推一次）
    last_notify: Mutex<Instant>,
}

const PUSH_INTERVAL: Duration = Duration::from_millis(800);

impl DownloadProgress {
    fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            path_to_key: RwLock::new(HashMap::new()),
            last_notify: Mutex::new(Instant::now()),
        }
    }

    /// 建立（或更新）一个下载任务。幂等：key 已存在时只刷新元信息，不清流状态。
    pub async fn begin_task(
        &self,
        key: &str,
        platform: &str,
        title: &str,
        phase: &str,
        file_name: &str,
    ) {
        let now = now_standard_string();
        {
            let mut tasks = self.tasks.write().await;
            let entry = tasks.entry(key.to_string()).or_insert_with(|| TaskState {
                key: key.to_string(),
                platform: platform.to_string(),
                title: title.to_string(),
                phase: phase.to_string(),
                file_name: file_name.to_string(),
                started_at: now.clone(),
                streams: HashMap::new(),
            });
            entry.platform = platform.to_string();
            entry.title = title.to_string();
            entry.phase = phase.to_string();
            entry.file_name = file_name.to_string();
        }
        self.notify_force().await;
    }

    /// 在任务下注册一个流（文件路径）。total_bytes 为 0 表示总大小未知，稍后由下载器补上。
    pub async fn attach_stream(&self, key: &str, path: &str, total_bytes: u64) {
        let mut changed = false;
        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.get_mut(key) {
                let stream = task
                    .streams
                    .entry(path.to_string())
                    .or_insert_with(StreamState::new);
                if total_bytes > 0 {
                    stream.total_bytes.store(total_bytes, Ordering::Relaxed);
                }
                changed = true;
            }
        }
        if changed {
            {
                let mut map = self.path_to_key.write().await;
                map.entry(path.to_string()).or_insert_with(|| key.to_string());
            }
            self.notify_force().await;
        }
    }

    /// 下载器补充/修正某个流的总大小（拿到 HEAD/探测结果后调用）。
    pub async fn set_stream_total(&self, path: &str, total_bytes: u64) {
        let key = self.path_to_key.read().await.get(path).cloned();
        let Some(key) = key else { return };
        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.get_mut(&key) {
                if let Some(stream) = task.streams.get_mut(path) {
                    stream.total_bytes.store(total_bytes, Ordering::Relaxed);
                }
            }
        }
        self.notify_force().await;
    }

    /// 下载器上报某个流当前已下载字节（取历史最大值）。
    pub async fn report_bytes(&self, path: &str, downloaded: u64) {
        let key = self.path_to_key.read().await.get(path).cloned();
        let Some(key) = key else { return };
        {
            let mut tasks = self.tasks.write().await;
            let Some(task) = tasks.get_mut(&key) else { return };
            let Some(stream) = task.streams.get_mut(path) else { return };
            let previous = stream.downloaded_bytes.load(Ordering::Relaxed);
            if downloaded <= previous {
                return;
            }
            stream.downloaded_bytes.store(downloaded, Ordering::Relaxed);
            let now = Instant::now();
            let mut last = stream.last_report.lock().await;
            let elapsed = now.duration_since(last.0).as_secs_f64();
            if elapsed > 0.05 {
                let instant_bps = ((downloaded - last.1) as f64 / elapsed) as u64;
                // 平滑：新值占 60%，历史占 40%，避免速度抖动
                let smoothed = if last.2 > 0 {
                    ((instant_bps as f64) * 0.6 + (last.2 as f64) * 0.4) as u64
                } else {
                    instant_bps
                };
                last.0 = now;
                last.1 = downloaded;
                last.2 = smoothed;
            }
        }
        self.notify_throttled().await;
    }

    /// 更新任务阶段（如“下载音频流”“合并中”）。
    pub async fn set_phase(&self, key: &str, phase: &str) {
        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.get_mut(key) {
                if task.phase != phase {
                    task.phase = phase.to_string();
                }
            }
        }
        self.notify_force().await;
    }

    /// 结束任务：移除任务及其所有流绑定。
    pub async fn finish_task(&self, key: &str) {
        let mut removed_paths = Vec::new();
        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.remove(key) {
                removed_paths = task.streams.keys().cloned().collect();
            }
        }
        if !removed_paths.is_empty() {
            let mut map = self.path_to_key.write().await;
            for path in removed_paths {
                if map.get(&path).is_some_and(|k| k == key) {
                    map.remove(&path);
                }
            }
            self.notify_force().await;
        }
    }

    /// 当前所有下载任务快照（按开始时间排序，旧的在前）。
    pub async fn snapshot(&self) -> Vec<DownloadProgressItem> {
        let tasks = self.tasks.read().await;
        let mut items = Vec::with_capacity(tasks.len());
        for task in tasks.values() {
            let mut downloaded = 0u64;
            let mut total = 0u64;
            let mut speed = 0u64;
            for stream in task.streams.values() {
                downloaded = downloaded.saturating_add(stream.downloaded_bytes.load(Ordering::Relaxed));
                total = total.saturating_add(stream.total_bytes.load(Ordering::Relaxed));
                speed = speed.saturating_add(stream.last_report.lock().await.2);
            }
            let eta_seconds = if speed > 0 && total > downloaded {
                Some(((total - downloaded) as f64 / speed as f64).ceil() as u64)
            } else {
                None
            };
            items.push(DownloadProgressItem {
                key: task.key.clone(),
                platform: task.platform.clone(),
                title: task.title.clone(),
                phase: task.phase.clone(),
                file_name: task.file_name.clone(),
                downloaded_bytes: downloaded,
                total_bytes: total,
                speed_bps: speed,
                eta_seconds,
                started_at: task.started_at.clone(),
            });
        }
        items.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        items
    }

    /// 强制推送：状态变化（开始/阶段切换/结束）必须让前端立即感知。
    async fn notify_force(&self) {
        *self.last_notify.lock().await = Instant::now();
        live_updates::notify_downloads_changed();
    }

    /// 节流推送：字节进度高频上报时限制推送频率。
    async fn notify_throttled(&self) {
        let mut last = self.last_notify.lock().await;
        if last.elapsed() >= PUSH_INTERVAL {
            *last = Instant::now();
            drop(last);
            live_updates::notify_downloads_changed();
        }
    }
}

/// 兜底：任务被异常跳过（未显式 finish）时，清理其绑定的流路径。
pub async fn cleanup_stale_paths(paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let mut map = DOWNLOAD_PROGRESS.path_to_key.write().await;
    let mut keys_to_finish = Vec::new();
    for path in paths {
        if let Some(key) = map.remove(path) {
            if !keys_to_finish.contains(&key) {
                keys_to_finish.push(key);
            }
        }
    }
    drop(map);
    for key in keys_to_finish {
        DOWNLOAD_PROGRESS.finish_task(&key).await;
    }
}

pub static DOWNLOAD_PROGRESS: Lazy<DownloadProgress> = Lazy::new(DownloadProgress::new);

fn now_standard_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_progress() -> DownloadProgress {
        DownloadProgress::new()
    }

    #[tokio::test]
    async fn attach_and_report_updates_snapshot() {
        let p = new_progress();
        p.begin_task("bilibili:1:2", "bilibili", "测试视频", "下载视频流", "test.mp4")
            .await;
        p.attach_stream("bilibili:1:2", r"F:\tmp\P1.tmp_video", 0).await;
        p.set_stream_total(r"F:\tmp\P1.tmp_video", 1000).await;
        p.report_bytes(r"F:\tmp\P1.tmp_video", 400).await;
        let snap = p.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].downloaded_bytes, 400);
        assert_eq!(snap[0].total_bytes, 1000);
        assert_eq!(snap[0].phase, "下载视频流");
        assert_eq!(snap[0].title, "测试视频");
        p.finish_task("bilibili:1:2").await;
        assert!(p.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn multi_stream_aggregates_bytes() {
        let p = new_progress();
        p.begin_task("bilibili:9:3", "bilibili", "多流", "下载视频流", "v.mp4")
            .await;
        p.attach_stream("bilibili:9:3", r"F:\tmp\v.tmp_video", 0).await;
        p.attach_stream("bilibili:9:3", r"F:\tmp\v.tmp_audio", 0).await;
        p.set_stream_total(r"F:\tmp\v.tmp_video", 800).await;
        p.set_stream_total(r"F:\tmp\v.tmp_audio", 200).await;
        p.report_bytes(r"F:\tmp\v.tmp_video", 500).await;
        p.report_bytes(r"F:\tmp\v.tmp_audio", 100).await;
        let snap = p.snapshot().await;
        assert_eq!(snap[0].downloaded_bytes, 600);
        assert_eq!(snap[0].total_bytes, 1000);
        // 未绑定路径的 report 不应影响任何任务
        p.report_bytes(r"F:\tmp\unknown.tmp_video", 999).await;
        assert_eq!(p.snapshot().await[0].downloaded_bytes, 600);
    }

    #[tokio::test]
    async fn phase_switch_is_visible() {
        let p = new_progress();
        p.begin_task("youtube:42", "youtube", "外源", "下载中", "").await;
        p.set_phase("youtube:42", "合并中").await;
        let snap = p.snapshot().await;
        assert_eq!(snap[0].phase, "合并中");
        assert_eq!(snap[0].platform, "youtube");
    }
}
