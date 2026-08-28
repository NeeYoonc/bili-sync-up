use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::WebSocketUpgrade;
use axum::response::IntoResponse;
use axum::routing::any;
use axum::Router;
use dashmap::DashMap;
use futures::stream::{SplitSink, SplitStream};
use futures::{future, SinkExt, StreamExt};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sysinfo::{
    get_current_pid, CpuRefreshKind, DiskKind, DiskRefreshKind, Disks, MemoryRefreshKind, ProcessRefreshKind,
    RefreshKind, System,
};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::{IntervalStream, WatchStream};
use tracing::{debug, error};
use uuid::Uuid;

use crate::api::response::SysInfo;
use crate::download_progress::DownloadProgressItem;
use crate::utils::live_updates;
use crate::utils::task_notifier::{TaskStatus, TASK_STATUS_NOTIFIER};

static WEBSOCKET_HANDLER: LazyLock<WebSocketHandler> = LazyLock::new(WebSocketHandler::new);

pub fn router() -> Router {
    Router::new().route("/api/ws", any(websocket_handler))
}

async fn websocket_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

// 事件类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum EventType {
    Tasks,
    SysInfo,
    Downloads,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ClientEvent {
    Subscribe(EventType),
    Unsubscribe(EventType),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum ServerEvent {
    Tasks(Arc<TaskStatus>),
    SysInfo(Arc<SysInfo>),
    Downloads(Arc<Vec<DownloadProgressItem>>),
}

struct WebSocketHandler {
    sysinfo_subscribers: Arc<DashMap<Uuid, tokio::sync::mpsc::Sender<ServerEvent>>>,
    sysinfo_handles: RwLock<Option<JoinHandle<()>>>,
}

impl WebSocketHandler {
    fn new() -> Self {
        Self {
            sysinfo_subscribers: Arc::new(DashMap::new()),
            sysinfo_handles: RwLock::new(None),
        }
    }

    async fn handle_sender(
        &self,
        mut sender: SplitSink<WebSocket, Message>,
        mut rx: tokio::sync::mpsc::Receiver<ServerEvent>,
    ) {
        while let Some(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(text) => {
                    if let Err(e) = sender.send(Message::Text(text.into())).await {
                        error!("Failed to send message: {:?}", e);
                        break;
                    }
                }
                Err(e) => {
                    error!("Failed to serialize event: {:?}", e);
                }
            }
        }
    }

    async fn handle_receiver(
        &self,
        mut receiver: SplitStream<WebSocket>,
        tx: tokio::sync::mpsc::Sender<ServerEvent>,
        uuid: Uuid,
    ) {
        let mut task_handle = None;
        let mut download_handle = None;
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                match serde_json::from_str::<ClientEvent>(&text) {
                    Ok(ClientEvent::Subscribe(event_type)) => match event_type {
                        EventType::Tasks => {
                            if task_handle.as_ref().is_none_or(|h: &JoinHandle<()>| h.is_finished()) {
                                let tx_clone = tx.clone();
                                task_handle = Some(tokio::spawn(async move {
                                    let mut stream =
                                        WatchStream::new(TASK_STATUS_NOTIFIER.subscribe()).map(ServerEvent::Tasks);
                                    while let Some(event) = stream.next().await {
                                        if let Err(e) = tx_clone.send(event).await {
                                            error!("Failed to send task status: {:?}", e);
                                            break;
                                        }
                                    }
                                }));
                            }
                        }
                        EventType::SysInfo => self.add_sysinfo_subscriber(uuid, tx.clone()).await,
                        EventType::Downloads => {
                            if download_handle
                                .as_ref()
                                .is_none_or(|h: &JoinHandle<()>| h.is_finished())
                            {
                                let tx_clone = tx.clone();
                                download_handle = Some(tokio::spawn(async move {
                                    let mut stream = live_updates::subscribe_downloads_changed();
                                    loop {
                                        let items = Arc::new(
                                            crate::download_progress::DOWNLOAD_PROGRESS.snapshot().await,
                                        );
                                        if tx_clone.send(ServerEvent::Downloads(items)).await.is_err() {
                                            break;
                                        }
                                        if stream.changed().await.is_err() {
                                            break;
                                        }
                                    }
                                }));
                            }
                        }
                    },
                    Ok(ClientEvent::Unsubscribe(event_type)) => match event_type {
                        EventType::Tasks => {
                            if let Some(handle) = task_handle.take() {
                                handle.abort();
                            }
                        }
                        EventType::SysInfo => {
                            self.remove_sysinfo_subscriber(uuid).await;
                        }
                        EventType::Downloads => {
                            if let Some(handle) = download_handle.take() {
                                handle.abort();
                            }
                        }
                    },
                    Err(e) => {
                        error!("Failed to parse client message: {:?}", e);
                    }
                }
            }
        }
        if let Some(handle) = task_handle {
            handle.abort();
        }
        if let Some(handle) = download_handle {
            handle.abort();
        }
        self.remove_sysinfo_subscriber(uuid).await;
    }

    // 添加订阅者
    async fn add_sysinfo_subscriber(&self, uuid: Uuid, sender: tokio::sync::mpsc::Sender<ServerEvent>) {
        self.sysinfo_subscribers.insert(uuid, sender);
        if !self.sysinfo_subscribers.is_empty()
            && self
                .sysinfo_handles
                .read()
                .as_ref()
                .is_none_or(|h: &JoinHandle<()>| h.is_finished())
        {
            let sysinfo_subscribers = self.sysinfo_subscribers.clone();
            let mut write_guard = self.sysinfo_handles.write();
            if write_guard.as_ref().is_some_and(|h: &JoinHandle<()>| !h.is_finished()) {
                return;
            }
            *write_guard = Some(tokio::spawn(async move {
                let mut system = System::new();
                let mut disks = Disks::new();
                let sys_refresh_kind = sys_refresh_kind();
                let disk_refresh_kind = disk_refresh_kind();
                // 对于 linux/mac/windows 平台，该方法永远返回 Some(pid)，expect 基本是安全的
                let self_pid = get_current_pid().expect("Unsupported platform");
                let mut first_run = true; // 用于控制只在首次进入时输出磁盘信息
                let mut stream =
                    IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).filter_map(move |_| {
                        system.refresh_specifics(sys_refresh_kind);
                        disks.refresh_specifics(true, disk_refresh_kind);
                        let process = match system.process(self_pid) {
                            Some(p) => p,
                            None => return futures::future::ready(None),
                        };
                        // 筛选 SSD/HDD 类型的磁盘，并去重
                        // Linux: 按设备名称（name）去重，因为同一设备可能有多个挂载点
                        // Windows: 按挂载点（mount_point）去重，因为设备名可能为空或重复

                        // 只在首次进入时输出磁盘信息
                        if first_run {
                            first_run = false;
                            debug!("=== 磁盘信息 ===");
                            debug!("磁盘总数: {}", disks.list().len());
                            for disk in disks.list() {
                                debug!(
                                    "磁盘: name={:?}, kind={:?}, mount_point={:?}, total={}, available={}",
                                    disk.name(),
                                    disk.kind(),
                                    disk.mount_point(),
                                    disk.total_space(),
                                    disk.available_space()
                                );
                            }
                            debug!("=== 磁盘信息结束 ===");
                        }

                        // 判断是否使用 name 去重（Linux）还是 mount_point 去重（Windows）
                        // 如果第一个磁盘的 name 不为空，则使用 name 去重
                        let use_name_dedup = disks.list().first().is_some_and(|d| !d.name().is_empty());

                        let mut seen_keys = HashSet::new();
                        let mut seen_total_spaces = HashSet::new(); // 按 total_space 去重（避免 overlay 和 device mapper 重复）
                        let (total_disk, available_disk) = {
                            // 先尝试只统计 SSD/HDD 类型的磁盘
                            let ssd_hdd_disks: Vec<_> = disks
                                .iter()
                                .filter(|d| matches!(d.kind(), DiskKind::SSD | DiskKind::HDD))
                                .filter(|d| {
                                    let key_unique = if use_name_dedup {
                                        seen_keys.insert(d.name().to_os_string())
                                    } else {
                                        seen_keys.insert(d.mount_point().as_os_str().to_os_string())
                                    };
                                    // 额外按 total_space 去重（Docker 环境下 overlay 和 /dev/mapper 可能指向同一存储）
                                    let space_unique = seen_total_spaces.insert(d.total_space());
                                    key_unique && space_unique
                                })
                                .collect();

                            if !ssd_hdd_disks.is_empty() {
                                // 有 SSD/HDD 磁盘，使用筛选后的结果
                                ssd_hdd_disks.iter().fold((0u64, 0u64), |(total, available), d| {
                                    (total + d.total_space(), available + d.available_space())
                                })
                            } else {
                                // 没有识别到 SSD/HDD，回退到所有磁盘（去重）
                                seen_keys.clear();
                                seen_total_spaces.clear();
                                disks
                                    .iter()
                                    .filter(|d| {
                                        let key_unique = if use_name_dedup {
                                            seen_keys.insert(d.name().to_os_string())
                                        } else {
                                            seen_keys.insert(d.mount_point().as_os_str().to_os_string())
                                        };
                                        // 额外按 total_space 去重
                                        let space_unique = seen_total_spaces.insert(d.total_space());
                                        key_unique && space_unique
                                    })
                                    .fold((0u64, 0u64), |(total, available), d| {
                                        (total + d.total_space(), available + d.available_space())
                                    })
                            }
                        };
                        futures::future::ready(Some(SysInfo {
                            total_memory: system.total_memory(),
                            used_memory: system.used_memory(),
                            process_memory: process.memory(),
                            used_cpu: system.global_cpu_usage(),
                            process_cpu: process.cpu_usage() / system.cpus().len() as f32,
                            total_disk,
                            available_disk,
                        }))
                    });
                while let Some(sys_info) = stream.next().await {
                    let sys_info = Arc::new(sys_info);
                    future::join_all(sysinfo_subscribers.iter().map(async |subscriber| {
                        if let Err(e) = subscriber.send(ServerEvent::SysInfo(sys_info.clone())).await {
                            error!(
                                "Failed to send sysinfo event to subscriber {}: {:?}",
                                subscriber.key(),
                                e
                            );
                        }
                    }))
                    .await;
                }
            }));
        }
    }

    async fn remove_sysinfo_subscriber(&self, uuid: Uuid) {
        self.sysinfo_subscribers.remove(&uuid);
        if self.sysinfo_subscribers.is_empty() {
            if let Some(handle) = self.sysinfo_handles.write().take() {
                handle.abort();
            }
        }
    }
}

async fn handle_socket(socket: WebSocket) {
    let (ws_sender, ws_receiver) = socket.split();
    let uuid = Uuid::new_v4();
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    tokio::spawn(WEBSOCKET_HANDLER.handle_sender(ws_sender, rx));
    tokio::spawn(WEBSOCKET_HANDLER.handle_receiver(ws_receiver, tx, uuid));
}

fn sys_refresh_kind() -> RefreshKind {
    RefreshKind::nothing()
        .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
        .with_memory(MemoryRefreshKind::nothing().with_ram())
        .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory())
}

fn disk_refresh_kind() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage()
}
