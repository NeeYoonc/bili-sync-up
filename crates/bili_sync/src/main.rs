#[macro_use]
extern crate tracing;

mod adapter;
mod api;
mod aria2_downloader;
mod auth;
mod bilibili;
mod config;
mod database;
mod downloader;
mod error;
mod hardware;
mod http;
mod ingest_log;
mod initialization;
mod task;
mod unified_downloader;
mod utils;
mod workflow;

use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

// 移除未使用的Lazy导入
use task::{http_server, video_downloader};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{init_config_with_database, ARGS, CONFIG_BUNDLE};
use crate::database::setup_database;
use crate::hardware::HardwareFingerprint;
use crate::utils::signal::terminate;
use crate::utils::{file_logger, init_logger};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    init();

    let connection = Arc::new(setup_database().await);

    // 初始化数据库配置系统
    if let Err(e) = init_config_with_database(connection.as_ref().clone()).await {
        warn!("数据库配置系统初始化失败: {}, 继续使用TOML配置", e);
    }

    // 启动时自动统一 upper_face 分桶目录大小写（A-Z -> a-z），避免媒体库头像匹配异常
    if let Err(e) = crate::workflow::migrate_upper_face_buckets_on_startup().await {
        warn!("启动时执行 upper_face 分桶迁移失败: {}", e);
    }

    // 初始化基于用户的硬件指纹系统
    if let Err(e) = init_hardware_fingerprint_for_user(&connection).await {
        debug!("硬件指纹初始化跳过: {}", e);
        // 无有效用户ID时跳过硬件指纹初始化，等待用户登录后再初始化
    }

    // 恢复断点信息到内存
    if let Err(e) = crate::utils::submission_checkpoint::restore_checkpoints_from_db(&connection).await {
        warn!("恢复断点信息失败: {:#}", e);
    }

    // 恢复待处理的任务到内存队列
    if let Err(e) = crate::task::recover_pending_tasks(connection.as_ref()).await {
        warn!("恢复待处理任务失败: {:#}", e);
    } else {
        // 立即串行处理恢复的任务
        info!("开始优先处理恢复的任务...");
        let mut total_processed = 0u32;

        // 1. 处理配置任务（优先级最高，影响其他任务）
        match crate::task::process_config_tasks(connection.clone()).await {
            Ok(count) => {
                total_processed += count;
                if count > 0 {
                    info!("处理了 {} 个恢复的配置任务", count);
                }
            }
            Err(e) => warn!("处理恢复的配置任务失败: {:#}", e),
        }

        // 2. 处理添加任务
        match crate::task::process_add_tasks(connection.clone()).await {
            Ok(count) => {
                total_processed += count;
                if count > 0 {
                    info!("处理了 {} 个恢复的添加任务", count);
                }
            }
            Err(e) => warn!("处理恢复的添加任务失败: {:#}", e),
        }

        // 3. 处理删除任务
        match crate::task::process_delete_tasks(connection.clone()).await {
            Ok(count) => {
                total_processed += count;
                if count > 0 {
                    info!("处理了 {} 个恢复的删除任务", count);
                }
            }
            Err(e) => warn!("处理恢复的删除任务失败: {:#}", e),
        }

        // 4. 处理视频删除任务
        match crate::task::process_video_delete_tasks(connection.clone()).await {
            Ok(count) => {
                total_processed += count;
                if count > 0 {
                    info!("处理了 {} 个恢复的视频删除任务", count);
                }
            }
            Err(e) => warn!("处理恢复的视频删除任务失败: {:#}", e),
        }

        if total_processed > 0 {
            info!("恢复的任务处理完成，共处理 {} 个任务", total_processed);
        } else {
            info!("没有需要处理的恢复任务");
        }
    }

    // SQLite配置已经在database::setup_database中设置了mmap，不再需要额外的初始化

    let token = CancellationToken::new();
    let tracker = TaskTracker::new();

    spawn_task("HTTP 服务", http_server(connection.clone()), &tracker, token.clone());
    spawn_task("定时下载", video_downloader(connection), &tracker, token.clone());

    tracker.close();
    handle_shutdown(tracker, token).await;
    Ok(())
}

fn spawn_task(
    task_name: &'static str,
    task: impl Future<Output = impl Debug> + Send + 'static,
    tracker: &TaskTracker,
    token: CancellationToken,
) {
    tracker.spawn(async move {
        tokio::select! {
            res = task => {
                error!("「{}」异常结束，返回结果为：「{:?}」，取消其它仍在执行的任务..", task_name, res);
                token.cancel();
            },
            _ = token.cancelled() => {
                info!("「{}」接收到取消信号，终止运行..", task_name);
            }
        }
    });
}

/// 初始化日志系统，打印欢迎信息
fn init() {
    // 强制初始化文件日志系统（这会设置启动时间）
    let _ = &*file_logger::STARTUP_TIME;
    let _ = &*file_logger::FILE_LOG_WRITER;

    init_logger(&ARGS.log_level);
    info!("欢迎使用 Bili-Sync，当前程序版本：{}", config::version());
    info!("现项目地址：https://github.com/NeeYoonc/bili-sync-up");
    info!("原项目地址：https://github.com/amtoaer/bili-sync");
    debug!("系统初始化完成，日志级别: {}", ARGS.log_level);

    // 硬件指纹初始化已移至main函数中的数据库初始化后
}

async fn handle_shutdown(tracker: TaskTracker, token: CancellationToken) {
    tokio::select! {
        _ = tracker.wait() => {
            error!("所有任务均已终止，程序退出");
            finalize_global_systems().await;
        }
        _ = terminate() => {
            info!("接收到终止信号，正在终止任务..");
            // 立即刷新日志以确保不丢失
            file_logger::flush_file_logger();

            // 在取消任务前先保存断点信息
            if let Some(db) = crate::database::get_global_db() {
                if let Err(e) = crate::utils::submission_checkpoint::save_checkpoints_to_db(&db).await {
                    warn!("终止时保存断点信息失败: {:#}", e);
                } else {
                    info!("终止时成功保存断点信息到数据库");
                }
            }

            token.cancel();
            tracker.wait().await;
            info!("所有任务均已终止，程序退出");
            finalize_global_systems().await;
        }
    }
}

/// 初始化基于用户的硬件指纹系统
async fn init_hardware_fingerprint_for_user(connection: &sea_orm::DatabaseConnection) -> Result<()> {
    debug!("开始初始化基于用户的硬件指纹系统");

    // 从配置中获取用户ID
    let config_bundle = CONFIG_BUNDLE.load();
    if let Some(credential) = config_bundle.config.credential.load_full() {
        if let Ok(user_id) = credential.dedeuserid.parse::<i64>() {
            info!("检测到用户ID: {}, 初始化硬件指纹", user_id);

            // 初始化该用户的硬件指纹
            HardwareFingerprint::init_global_for_user(user_id, connection).await?;

            info!("用户 {} 的硬件指纹初始化完成", user_id);
        } else {
            debug!("无效的用户ID格式: {}，跳过硬件指纹初始化", credential.dedeuserid);
            return Err(anyhow::anyhow!("无效的用户ID格式，等待用户登录"));
        }
    } else {
        debug!("未找到有效的用户凭据，跳过硬件指纹初始化");
        return Err(anyhow::anyhow!("未找到有效的用户凭据，等待用户登录"));
    }

    Ok(())
}

/// 完成全局系统清理
async fn finalize_global_systems() {
    // 最后一次保存断点信息（双重保险）
    if let Some(db) = crate::database::get_global_db() {
        if let Err(e) = crate::utils::submission_checkpoint::save_checkpoints_to_db(&db).await {
            warn!("最终保存断点信息失败: {:#}", e);
        }
    }

    // SQLite会自动处理mmap的清理，不需要额外的finalize操作

    // 关闭文件日志系统
    file_logger::shutdown_file_logger();
}
