use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::utils::time_format::now_standard_string;

/// 入库状态
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    /// 下载成功
    Success,
    /// 下载失败（重试后仍失败）
    Failed,
    /// 视频已被删除
    Deleted,
}

/// 首页「最新入库」展示用的内存事件（环形缓冲）。
///
/// 说明：
/// - 事件只在进程生命周期内保留（不做 DB 迁移，避免破坏性变更）。
/// - 下载速度为"媒体流下载平均速度"（视频/音频流累计）。
/// - 当视频无需下载（已存在）或没有可用时长时，速度为 None。

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEvent {
    pub video_id: i32,
    pub video_name: String,
    pub upper_name: String,
    pub path: String,
    pub ingested_at: String,
    pub download_speed_bps: Option<u64>,
    pub status: IngestStatus,
    /// 番剧系列名称（从share_copy的《》中提取）
    pub series_name: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Accumulator {
    bytes: u64,
    millis: u64,
}

impl Accumulator {
    fn add(&mut self, bytes: u64, elapsed: Duration) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.millis = self
            .millis
            .saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
    }

    fn avg_bps(&self) -> Option<u64> {
        if self.bytes == 0 || self.millis == 0 {
            return None;
        }
        // bytes / seconds
        let secs = self.millis as f64 / 1000.0;
        if secs <= 0.0 {
            return None;
        }
        Some((self.bytes as f64 / secs) as u64)
    }
}

pub struct IngestLog {
    max_len: usize,
    events: Mutex<VecDeque<IngestEvent>>,
    accumulators: Mutex<HashMap<i32, Accumulator>>,
}

impl IngestLog {
    pub fn new(max_len: usize) -> Self {
        Self {
            max_len,
            events: Mutex::new(VecDeque::new()),
            accumulators: Mutex::new(HashMap::new()),
        }
    }

    /// 记录一次媒体流下载（用于计算平均速度）
    pub async fn add_download_sample(&self, video_id: i32, bytes: u64, elapsed: Duration) {
        let mut map = self.accumulators.lock().await;
        let entry = map.entry(video_id).or_default();
        entry.add(bytes, elapsed);
    }

    /// 完成一个视频的入库事件（会消费并清理该 video_id 的累计下载统计）
    pub async fn finish_video(
        &self,
        video_id: i32,
        video_name: String,
        upper_name: String,
        path: String,
        status: IngestStatus,
        series_name: Option<String>,
    ) {
        let download_speed_bps = {
            let mut map = self.accumulators.lock().await;
            map.remove(&video_id).and_then(|a| a.avg_bps())
        };

        let mut q = self.events.lock().await;
        q.push_front(IngestEvent {
            video_id,
            video_name,
            upper_name,
            path,
            ingested_at: now_standard_string(),
            download_speed_bps,
            status,
            series_name,
        });

        while q.len() > self.max_len {
            q.pop_back();
        }
    }

    pub async fn list_latest(&self, limit: usize) -> Vec<IngestEvent> {
        let q = self.events.lock().await;
        q.iter().take(limit).cloned().collect()
    }
}

pub static INGEST_LOG: Lazy<IngestLog> = Lazy::new(|| IngestLog::new(200));
