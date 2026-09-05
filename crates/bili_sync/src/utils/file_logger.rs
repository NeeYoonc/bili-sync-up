use crate::config::CONFIG_DIR;
use chrono::{Local, TimeZone};
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// 向后兼容：全局启动时间，用于其他地方的引用
pub static STARTUP_TIME: Lazy<String> = Lazy::new(|| Local::now().format("%Y-%m-%d-%H-%M-%S").to_string());

static SKIP_FIRST_ROUND_LOG_ROTATE: AtomicBool = AtomicBool::new(true);

// 日志条目结构
#[derive(Debug)]
struct LogEntry {
    timestamp: String,
    level: String,
    message: String,
    target: String,
}

// 日志文件写入器
pub struct FileLogWriter {
    all_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    debug_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    info_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    warn_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    error_writer: Arc<Mutex<Option<BufWriter<File>>>>,
    // 日志缓冲区
    log_buffer: Arc<Mutex<VecDeque<LogEntry>>>,
    // 当前日志文件标识（每轮生成一个新文件）
    current_log_id: Arc<Mutex<String>>,
    // 日志目录
    log_dir: std::path::PathBuf,
}

impl FileLogWriter {
    pub fn new() -> anyhow::Result<Self> {
        // 创建日志目录
        let log_dir = CONFIG_DIR.join("logs");
        fs::create_dir_all(&log_dir)?;

        // 清理超过30天的旧日志
        Self::cleanup_old_logs(&log_dir)?;

        // 初始化日志文件标识（启动时生成一份，后续每轮扫描会轮转）
        let initial_log_id = STARTUP_TIME.clone();

        let instance = Self {
            all_writer: Arc::new(Mutex::new(None)),
            debug_writer: Arc::new(Mutex::new(None)),
            info_writer: Arc::new(Mutex::new(None)),
            warn_writer: Arc::new(Mutex::new(None)),
            error_writer: Arc::new(Mutex::new(None)),
            log_buffer: Arc::new(Mutex::new(VecDeque::new())),
            current_log_id: Arc::new(Mutex::new(initial_log_id.clone())),
            log_dir: log_dir.clone(),
        };

        // 创建启动时的日志文件
        instance.create_round_log_files(&initial_log_id)?;

        Ok(instance)
    }

    fn create_round_log_files(&self, log_id: &str) -> anyhow::Result<()> {
        let all_path = self.log_dir.join(format!("logs-all-{}.csv", log_id));
        let debug_path = self.log_dir.join(format!("logs-debug-{}.csv", log_id));
        let info_path = self.log_dir.join(format!("logs-info-{}.csv", log_id));
        let warn_path = self.log_dir.join(format!("logs-warn-{}.csv", log_id));
        let error_path = self.log_dir.join(format!("logs-error-{}.csv", log_id));

        // 创建文件并写入CSV头
        let all_writer = Self::create_log_file(&all_path)?;
        let debug_writer = Self::create_log_file(&debug_path)?;
        let info_writer = Self::create_log_file(&info_path)?;
        let warn_writer = Self::create_log_file(&warn_path)?;
        let error_writer = Self::create_log_file(&error_path)?;

        // 更新写入器
        *self.all_writer.lock().unwrap() = Some(all_writer);
        *self.debug_writer.lock().unwrap() = Some(debug_writer);
        *self.info_writer.lock().unwrap() = Some(info_writer);
        *self.warn_writer.lock().unwrap() = Some(warn_writer);
        *self.error_writer.lock().unwrap() = Some(error_writer);

        Ok(())
    }

    fn generate_unique_log_id(&self) -> String {
        let base = Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let mut candidate = base.clone();
        let mut index = 1;

        loop {
            let all_path = self.log_dir.join(format!("logs-all-{}.csv", candidate));
            if !all_path.exists() {
                return candidate;
            }
            candidate = format!("{}-{}", base, index);
            index += 1;
        }
    }

    pub fn rotate_for_new_round(&self) -> anyhow::Result<String> {
        // 先把上一轮剩余缓冲刷到旧文件，避免跨轮污染
        self.flush_internal();

        let new_id = self.generate_unique_log_id();
        self.create_round_log_files(&new_id)?;
        *self.current_log_id.lock().unwrap() = new_id.clone();
        let _ = Self::cleanup_old_logs(&self.log_dir);
        Ok(new_id)
    }

    pub fn current_log_file_name(&self, level: &str) -> String {
        let id = self.current_log_id.lock().unwrap().clone();
        match level {
            "debug" => format!("logs-debug-{}.csv", id),
            "info" => format!("logs-info-{}.csv", id),
            "warn" => format!("logs-warn-{}.csv", id),
            "error" => format!("logs-error-{}.csv", id),
            _ => format!("logs-all-{}.csv", id),
        }
    }

    fn create_log_file(path: &Path) -> anyhow::Result<BufWriter<File>> {
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(path)?;

        let mut buf_writer = BufWriter::with_capacity(64 * 1024, file); // 64KB缓冲区

        // 写入CSV头，使用UTF-8 BOM以支持Excel正确识别中文
        buf_writer.write_all(&[0xEF, 0xBB, 0xBF])?; // UTF-8 BOM
        writeln!(buf_writer, "时间,级别,消息,来源")?;
        buf_writer.flush()?; // 仅刷新缓冲区，不强制同步到磁盘

        Ok(buf_writer)
    }

    fn cleanup_old_logs(log_dir: &Path) -> anyhow::Result<()> {
        // 保留最近 7 天（与“数据库管理 → 清理旧日志”的手动策略一致）
        let keep_from_date = Local::now().date_naive() - chrono::Duration::days(6);

        if let Ok(entries) = fs::read_dir(log_dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(modified_datetime) = modified.duration_since(std::time::UNIX_EPOCH) {
                                let modified_timestamp = modified_datetime.as_secs() as i64;
                                // 使用 timestamp_opt 方法来创建本地时间
                                let modified_datetime = Local
                                    .timestamp_opt(modified_timestamp, 0)
                                    .single()
                                    .unwrap_or_else(Local::now);

                                if modified_datetime.date_naive() < keep_from_date {
                                    // 删除保留期外的旧日志文件
                                    let _ = fs::remove_file(entry.path());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn write_log(&self, timestamp: &str, level: &str, message: &str, target: Option<&str>) {
        let target_str = target.unwrap_or("");

        // 创建日志条目
        let entry = LogEntry {
            timestamp: timestamp.to_string(),
            level: level.to_string(),
            message: message.to_string(),
            target: target_str.to_string(),
        };

        // 添加到缓冲区，非阻塞操作
        if let Ok(mut buffer) = self.log_buffer.lock() {
            buffer.push_back(entry);
            // 当缓冲区达到1000条时自动刷新到文件
            if buffer.len() >= 1000 {
                // 先取出所有日志，避免死锁
                let entries: Vec<LogEntry> = buffer.drain(..).collect();
                drop(buffer); // 释放锁

                // 写入文件
                self.write_entries_to_files(entries);
            }
        }
    }

    fn escape_csv(field: &str) -> String {
        if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
            format!("\"{}\"", field.replace('"', "\"\""))
        } else {
            field.to_string()
        }
    }

    // 内部方法：直接写入日志条目到文件
    fn write_entries_to_files(&self, entries: Vec<LogEntry>) {
        for entry in entries {
            let escaped_message = Self::escape_csv(&entry.message);
            let escaped_target = Self::escape_csv(&entry.target);
            let log_line = format!(
                "{},{},{},{}\n",
                entry.timestamp, entry.level, escaped_message, escaped_target
            );

            // 写入全部日志文件（不包含debug级别）
            if entry.level.to_lowercase() != "debug" {
                if let Ok(mut writer_opt) = self.all_writer.lock() {
                    if let Some(ref mut writer) = writer_opt.as_mut() {
                        let _ = writer.write_all(log_line.as_bytes());
                        let _ = writer.flush(); // 立即刷新
                    }
                }
            }

            // 根据级别写入对应文件
            let level_writer = match entry.level.to_lowercase().as_str() {
                "debug" => &self.debug_writer,
                "info" => &self.info_writer,
                "warn" => &self.warn_writer,
                "error" => &self.error_writer,
                _ => continue,
            };

            if let Ok(mut writer_opt) = level_writer.lock() {
                if let Some(ref mut writer) = writer_opt.as_mut() {
                    let _ = writer.write_all(log_line.as_bytes());
                    let _ = writer.flush(); // 立即刷新
                }
            }
        }
    }

    // 手动刷新日志到文件
    pub fn flush(&self) {
        self.flush_internal();
    }

    // 内部刷新方法
    fn flush_internal(&self) {
        // 获取待处理的日志
        let entries = {
            if let Ok(mut buffer) = self.log_buffer.lock() {
                let entries: Vec<LogEntry> = buffer.drain(..).collect();
                entries
            } else {
                Vec::new()
            }
        };

        // 写入日志条目
        if !entries.is_empty() {
            self.write_entries_to_files(entries);
        }

        // 刷新所有写入器的缓冲区
        self.flush_all_writers();
    }

    // 刷新所有写入器
    fn flush_all_writers(&self) {
        if let Ok(mut writer_opt) = self.all_writer.lock() {
            if let Some(ref mut writer) = writer_opt.as_mut() {
                let _ = writer.flush();
                let _ = writer.get_mut().sync_all();
            }
        }
        if let Ok(mut writer_opt) = self.debug_writer.lock() {
            if let Some(ref mut writer) = writer_opt.as_mut() {
                let _ = writer.flush();
                let _ = writer.get_mut().sync_all();
            }
        }
        if let Ok(mut writer_opt) = self.info_writer.lock() {
            if let Some(ref mut writer) = writer_opt.as_mut() {
                let _ = writer.flush();
                let _ = writer.get_mut().sync_all();
            }
        }
        if let Ok(mut writer_opt) = self.warn_writer.lock() {
            if let Some(ref mut writer) = writer_opt.as_mut() {
                let _ = writer.flush();
                let _ = writer.get_mut().sync_all();
            }
        }
        if let Ok(mut writer_opt) = self.error_writer.lock() {
            if let Some(ref mut writer) = writer_opt.as_mut() {
                let _ = writer.flush();
                let _ = writer.get_mut().sync_all();
            }
        }
    }

    // 优雅停止
    pub fn shutdown(&self) {
        // 最后一次刷新所有缓冲的日志
        self.flush();
    }
}

// 全局文件日志写入器
pub static FILE_LOG_WRITER: Lazy<Option<FileLogWriter>> = Lazy::new(|| match FileLogWriter::new() {
    Ok(writer) => Some(writer),
    Err(e) => {
        tracing::error!("文件日志系统初始化失败: {}", e);
        None
    }
});

// 手动刷新所有缓冲的日志到文件
pub fn flush_file_logger() {
    if let Some(ref writer) = *FILE_LOG_WRITER {
        writer.flush();
    }
}

/// 每轮扫描开始时调用，生成新的日志文件
pub fn rotate_log_files_for_new_round() -> Option<String> {
    if let Some(ref writer) = *FILE_LOG_WRITER {
        if SKIP_FIRST_ROUND_LOG_ROTATE.swap(false, Ordering::Relaxed) {
            writer.flush();
            return None;
        }
        match writer.rotate_for_new_round() {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!("轮转日志文件失败: {}", e);
                None
            }
        }
    } else {
        None
    }
}

pub fn get_current_log_file_name(level: &str) -> Option<String> {
    if let Some(ref writer) = *FILE_LOG_WRITER {
        Some(writer.current_log_file_name(level))
    } else {
        None
    }
}

/// 手动清理旧日志文件：保留最近 `keep_days` 天（含今天）的日志，删除更早的文件。
/// 会跳过当前正在写入的日志轮次（Windows 下打开的文件无法删除）。返回删除的文件数量。
pub fn clean_old_log_files(keep_days: u32) -> usize {
    let Some(ref writer) = *FILE_LOG_WRITER else {
        return 0;
    };
    let current_id = writer.current_log_id.lock().unwrap().clone();
    // 今天算第 1 天：keep_days=7 时保留今天与最近 6 天
    let keep_from = Local::now().date_naive() - chrono::Duration::days(i64::from(keep_days.saturating_sub(1)));
    let mut removed = 0usize;
    let Ok(entries) = fs::read_dir(&writer.log_dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        // 跳过当前正在写入的日志轮次
        if file_name.contains(&current_id) {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(modified_since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let modified_datetime = Local
            .timestamp_opt(modified_since_epoch.as_secs() as i64, 0)
            .single()
            .unwrap_or_else(Local::now);
        if modified_datetime.date_naive() < keep_from && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

// 在程序退出时调用，确保所有日志都被写入
pub fn shutdown_file_logger() {
    if let Some(ref writer) = *FILE_LOG_WRITER {
        writer.shutdown();
    }
}
