use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Path, Request};
use axum::http::{header, Uri};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{middleware, Extension, Router, ServiceExt};
use reqwest::StatusCode;
use rust_embed::Embed;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use utoipa::OpenApi;
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::api::auth;
use crate::api::handler::{
    add_video_source,
    ai_rename_history,
    batch_update_config_internal,
    cancel_queue_task,
    check_initial_setup,
    clear_ai_rename_cache,
    clear_ai_rename_cache_for_source,
    clear_credential,
    delete_video,
    delete_video_source,
    download_log_file,
    generate_qr_code,
    get_bangumi_seasons,
    get_bangumi_sources_for_merge,
    get_beta_image_update_status,
    get_config,
    get_config_history,
    // 新增配置管理API
    get_config_item,
    get_config_migration_status,
    get_current_user,
    get_dashboard_data,
    get_hot_reload_status,
    get_latest_ingests,
    get_log_files,
    get_logs,
    get_notification_config,
    get_notification_status,
    get_queue_status,
    get_submission_videos,
    get_subscribed_collections,
    get_task_control_status,
    get_user_collections,
    get_user_favorites,
    get_user_favorites_by_uid,
    get_user_followings,
    get_video,
    get_video_bvid,
    get_video_play_info,
    get_video_source_keyword_filters,
    get_video_sources,
    get_videos,
    migrate_config_schema,
    pause_scanning_endpoint,
    poll_qr_status,
    proxy_image,
    proxy_video_stream,
    refresh_scanning_endpoint,
    reload_config,
    reload_config_new_internal,
    reset_all_videos,
    reset_specific_tasks,
    reset_video,
    reset_video_source_path,
    resume_scanning_endpoint,
    search_bilibili,
    setup_auth_token,
    test_notification_handler,
    test_risk_control_handler,
    update_config,
    update_config_item_internal,
    update_credential,
    update_notification_config,
    update_submission_selected_videos,
    update_video_source_download_options,
    update_video_source_enabled,
    update_video_source_keyword_filters,
    update_video_source_scan_deleted,
    update_video_status,
    validate_config,
    validate_favorite,
    validate_regex_pattern,
    ApiDoc,
};
use crate::api::request::{BatchUpdateConfigRequest, UpdateConfigItemRequest};
use crate::api::video_stream::stream_video;
use crate::api::wrapper::ApiResponse;
use crate::api::ws;
use crate::bilibili::{get_captcha_info, serve_captcha_page, submit_captcha_result};
// CONFIG导入已移除 - 现在使用动态配置

#[derive(Embed)]
#[folder = "../../web/build"]
struct Asset;

/// 测试数据库连接是否有效
async fn test_db_connection(db: &DatabaseConnection) -> bool {
    match db
        .execute(Statement::from_string(DatabaseBackend::Sqlite, "SELECT 1"))
        .await
    {
        Ok(_) => {
            debug!("数据库连接测试成功");

            // 进一步检查关键表是否存在
            let table_check_sql = "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('collection','favorite','submission','watch_later','video_source','video','page')";
            match db
                .query_all(Statement::from_string(DatabaseBackend::Sqlite, table_check_sql))
                .await
            {
                Ok(tables) => {
                    debug!("数据库中发现 {} 个关键表", tables.len());
                    if tables.len() < 7 {
                        warn!("数据库连接有效但缺少关键表，发现表数量: {}/7", tables.len());
                        false
                    } else {
                        true
                    }
                }
                Err(e) => {
                    warn!("检查数据库表失败: {}", e);
                    false
                }
            }
        }
        Err(e) => {
            warn!("数据库连接测试失败: {}", e);
            false
        }
    }
}

pub async fn http_server(_database_connection: Arc<DatabaseConnection>) -> Result<()> {
    // 使用主数据库连接
    let optimized_connection = {
        debug!("使用主数据库连接");

        // 验证主数据库连接
        if test_db_connection(&_database_connection).await {
            debug!("主数据库连接验证成功");
        } else {
            warn!("主数据库连接验证失败，HTTP服务器可能无法正常工作");
        }

        _database_connection
    };
    let app = Router::new()
        .route("/api/video-sources", get(get_video_sources))
        .route("/api/video-sources", post(add_video_source))
        .route("/api/video-sources/bangumi/list", get(get_bangumi_sources_for_merge))
        .route(
            "/api/video-sources/{source_type}/{id}/enabled",
            put(update_video_source_enabled),
        )
        .route(
            "/api/video-sources/{source_type}/{id}/scan-deleted",
            put(update_video_source_scan_deleted),
        )
        .route(
            "/api/video-sources/{source_type}/{id}/download-options",
            put(update_video_source_download_options),
        )
        .route(
            "/api/video-sources/{source_type}/{id}/reset-path",
            post(reset_video_source_path),
        )
        .route("/api/video-sources/{source_type}/{id}", delete(delete_video_source))
        .route(
            "/api/video-sources/submission/{id}/selected-videos",
            put(update_submission_selected_videos),
        )
        .route(
            "/api/video-sources/{source_type}/{id}/keyword-filters",
            put(update_video_source_keyword_filters).get(get_video_source_keyword_filters),
        )
        .route("/api/validate-regex", post(validate_regex_pattern))
        .route("/api/ai-rename/clear-cache", post(clear_ai_rename_cache))
        .route("/api/ai-rename/clear-cache/{source_type}/{id}", post(clear_ai_rename_cache_for_source))
        .route("/api/{source_type}/{id}/ai-rename-history", post(ai_rename_history))
        .route("/api/videos", get(get_videos))
        .route("/api/videos/{id}", get(get_video))
        .route("/api/videos/{id}", delete(delete_video))
        .route("/api/videos/{id}/reset", post(reset_video))
        .route("/api/videos/{id}/update-status", post(update_video_status))
        .route("/api/videos/reset-all", post(reset_all_videos))
        .route("/api/videos/reset-specific-tasks", post(reset_specific_tasks))
        .route("/api/dashboard", get(get_dashboard_data))
        .route("/api/reload-config", post(reload_config))
        .route("/api/config", get(get_config))
        .route("/api/config", put(update_config))
        // 新的配置管理API路由
        .route("/api/config/item/{key}", get(get_config_item))
        .route(
            "/api/config/item/{key}",
            put(
                |Path(key): Path<String>,
                 Extension(db): Extension<Arc<DatabaseConnection>>,
                 axum::Json(request): axum::Json<UpdateConfigItemRequest>| async move {
                    update_config_item_internal(db, key, request).await.map(ApiResponse::ok)
                },
            ),
        )
        .route(
            "/api/config/batch",
            post(
                |Extension(db): Extension<Arc<DatabaseConnection>>,
                 axum::Json(request): axum::Json<BatchUpdateConfigRequest>| async move {
                    batch_update_config_internal(db, request).await.map(ApiResponse::ok)
                },
            ),
        )
        .route(
            "/api/config/reload",
            post(|Extension(db): Extension<Arc<DatabaseConnection>>| async move {
                reload_config_new_internal(db).await.map(ApiResponse::ok)
            }),
        )
        .route("/api/config/history", get(get_config_history))
        .route("/api/config/migration/status", get(get_config_migration_status))
        .route("/api/config/migrate", post(migrate_config_schema))
        .route("/api/config/validate", post(validate_config))
        .route("/api/config/hot-reload/status", get(get_hot_reload_status))
        // 初始设置API路由
        .route("/api/setup/check", get(check_initial_setup))
        .route("/api/setup/auth-token", post(setup_auth_token))
        .route("/api/credential", put(update_credential))
        // 扫码登录API路由
        .route("/api/auth/qr/generate", post(generate_qr_code))
        .route("/api/auth/qr/poll", get(poll_qr_status))
        .route("/api/auth/current-user", get(get_current_user))
        .route("/api/auth/clear-credential", post(clear_credential))
        .route("/api/bangumi/seasons/{season_id}", get(get_bangumi_seasons))
        .route("/api/search", get(search_bilibili))
        .route("/api/user/favorites", get(get_user_favorites))
        .route("/api/user/{uid}/favorites", get(get_user_favorites_by_uid))
        .route("/api/favorite/{fid}/validate", get(validate_favorite))
        .route("/api/user/collections/{mid}", get(get_user_collections))
        .route("/api/user/followings", get(get_user_followings))
        .route("/api/user/subscribed-collections", get(get_subscribed_collections))
        .route("/api/submission/{up_id}/videos", get(get_submission_videos))
        .route("/api/logs", get(get_logs))
        .route("/api/logs/files", get(get_log_files))
        .route("/api/logs/download", get(download_log_file))
        .route("/api/queue-status", get(get_queue_status))
        .route("/api/queue/tasks/{task_id}", delete(cancel_queue_task))
        .route("/api/proxy/image", get(proxy_image))
        .route("/api/task-control/status", get(get_task_control_status))
        .route("/api/task-control/pause", post(pause_scanning_endpoint))
        .route("/api/task-control/resume", post(resume_scanning_endpoint))
        .route("/api/task-control/refresh", post(refresh_scanning_endpoint))
        .route("/api/ingest/latest", get(get_latest_ingests))
        // 推送通知API
        .route("/api/notification/test", post(test_notification_handler))
        .route("/api/config/notification", get(get_notification_config))
        .route("/api/config/notification", post(update_notification_config))
        .route("/api/notification/status", get(get_notification_status))
        // 测试API
        .route("/api/test/risk-control", post(test_risk_control_handler))
        // 视频流API
        .route("/api/videos/stream/{video_id}", get(stream_video))
        // 新增在线播放API
        .route("/api/videos/{video_id}/play-info", get(get_video_play_info))
        .route("/api/videos/{video_id}/bvid", get(get_video_bvid))
        .route("/api/videos/proxy-stream", get(proxy_video_stream))
        // beta 镜像更新检查（前端角标提示）
        .route("/api/updates/beta", get(get_beta_image_update_status))
        // 验证码相关API
        .route("/captcha", get(serve_captcha_page))
        .route("/api/captcha/info", get(get_captcha_info))
        .route("/api/captcha/submit", post(submit_captcha_result))
        // 先应用认证中间件
        .layer(Extension(optimized_connection.clone()))
        .layer(middleware::from_fn(auth::auth))
        // WebSocket API需要在认证中间件之后
        .merge(ws::router())
        .merge(
            SwaggerUi::new("/swagger-ui/")
                .url("/api-docs/openapi.json", ApiDoc::openapi())
                .config(
                    Config::default()
                        .try_it_out_enabled(true)
                        .persist_authorization(true)
                        .validator_url("none"),
                ),
        )
        .fallback_service(get(frontend_files));
    // 使用动态配置而非静态CONFIG
    // 启动周期性数据库连接健康检查
    let health_check_connection = optimized_connection.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(120)); // 每2分钟检查一次，更快发现问题
        loop {
            interval.tick().await;

            if !test_db_connection(&health_check_connection).await {
                error!("数据库连接健康检查失败！HTTP API可能无法正常工作");

                // mmap模式下不会有表丢失问题，直接报告错误
                error!("数据库连接问题，建议检查数据库状态或重启服务");
            } else {
                debug!("数据库连接健康检查通过");
            }
        }
    });

    let config = crate::config::reload_config();
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .context("bind address failed")?;
    info!("开始运行管理页: http://{}", config.bind_address);
    Ok(axum::serve(listener, ServiceExt::<Request>::into_make_service(app)).await?)
}

async fn frontend_files(uri: Uri) -> impl IntoResponse {
    let mut path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        path = "index.html";
    }

    match Asset::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response()
        }
        None => {
            // 对于SPA路由，如果请求的路径不是文件（没有扩展名），则返回index.html
            if !path.contains('.') {
                match Asset::get("index.html") {
                    Some(content) => {
                        let mime = mime_guess::from_path("index.html").first_or_octet_stream();
                        ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response()
                    }
                    None => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
                }
            } else {
                (StatusCode::NOT_FOUND, "404 Not Found").into_response()
            }
        }
    }
}
