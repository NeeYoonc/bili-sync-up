use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, warn};

use crate::adapter::{VideoSource, VideoSourceEnum};
use crate::utils::notification::{NewVideoInfo, ScanSummary, SourceScanResult};

/// 扫描收集器，用于收集每次完整扫描的统计信息
pub struct ScanCollector {
    start_time: Instant,
    source_results: HashMap<String, SourceScanResult>,
    total_sources: usize,
}

impl ScanCollector {
    /// 创建新的扫描收集器
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            source_results: HashMap::new(),
            total_sources: 0,
        }
    }

    /// 记录一个视频源的扫描开始
    pub fn start_source(&mut self, video_source: &VideoSourceEnum) {
        self.total_sources += 1;

        let key = self.get_source_key(video_source);
        let result = SourceScanResult {
            source_type: video_source.source_type_display(),
            source_name: video_source.source_name_display(),
            new_videos: Vec::new(),
        };

        self.source_results.insert(key, result);
    }

    /// 记录新增的视频信息
    pub fn add_new_video(&mut self, video_source: &VideoSourceEnum, video_info: NewVideoInfo) {
        let key = self.get_source_key(video_source);
        if let Some(result) = self.source_results.get_mut(&key) {
            result.new_videos.push(video_info);
        }
    }

    /// 批量添加新增视频信息
    pub fn add_new_videos(&mut self, video_source: &VideoSourceEnum, videos: Vec<NewVideoInfo>) {
        let key = self.get_source_key(video_source);
        debug!(
            "scan_collector.add_new_videos: key={}, videos.len()={}",
            key,
            videos.len()
        );

        if self.source_results.contains_key(&key) {
            let before_count = self
                .source_results
                .get(&key)
                .map(|result| result.new_videos.len())
                .unwrap_or(0);

            for video_info in videos {
                self.add_new_video(video_source, video_info);
            }

            let after_count = self
                .source_results
                .get(&key)
                .map(|result| result.new_videos.len())
                .unwrap_or(before_count);

            debug!(
                "scan_collector更新: {} 从{}个视频增加到{}个",
                key, before_count, after_count
            );
        } else {
            warn!("scan_collector.add_new_videos: 未找到源 {}", key);
        }
    }

    /// 生成扫描摘要
    pub fn generate_summary(self) -> ScanSummary {
        let scan_duration = self.start_time.elapsed();
        let total_new_videos = self.total_new_videos();

        debug!(
            "scan_collector.generate_summary: total_sources={}, total_new_videos={}",
            self.total_sources, total_new_videos
        );

        // 详细记录每个源的新视频数量
        for (key, result) in &self.source_results {
            if !result.new_videos.is_empty() {
                debug!("  源 '{}' 有 {} 个新视频", key, result.new_videos.len());
            }
        }

        let source_results = self.source_results.into_values().collect();

        ScanSummary {
            total_sources: self.total_sources,
            total_new_videos,
            scan_duration,
            source_results,
        }
    }

    /// 获取当前总的新增视频数量
    pub fn total_new_videos(&self) -> usize {
        self.source_results.values().map(|result| result.new_videos.len()).sum()
    }

    /// 生成视频源的唯一键
    fn get_source_key(&self, video_source: &VideoSourceEnum) -> String {
        format!(
            "{}:{}",
            video_source.source_type_display(),
            video_source.source_name_display()
        )
    }
}

impl Default for ScanCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// 从视频信息创建NewVideoInfo结构
pub fn create_new_video_info(title: &str, bvid: &str) -> NewVideoInfo {
    NewVideoInfo {
        title: title.to_string(),
        bvid: bvid.to_string(),
        pubtime: None,
        episode_number: None,
        video_id: None,
        url: None,
    }
}
