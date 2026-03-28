use std::collections::HashMap;

use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::StatusCode;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::Modify;

use crate::api::wrapper::ApiResponse;

pub async fn auth(headers: HeaderMap, request: Request, next: Next) -> Result<Response, StatusCode> {
    // 排除不需要认证的路径
    let path = request.uri().path();
    tracing::debug!("认证中间件: 检查路径 {}", path);

    let excluded_paths = [
        "/api/search",                // 搜索API不需要认证
        "/api/proxy/image",           // 图片代理不需要认证
        "/api/setup/check",           // 初始设置检查不需要认证
        "/api/setup/auth-token",      // 设置auth token不需要认证
        "/api/credential",            // 更新凭证在初始设置时不需要认证
        "/api/videos/stream",         // 视频流API不需要认证（供播放器使用）
        "/api/videos/proxy-stream",   // 视频流代理API不需要认证（供在线播放器使用）
        "/api/auth/qr/generate",      // 生成登录二维码不需要认证
        "/api/auth/qr/poll",          // 轮询登录状态不需要认证
        "/api/auth/current-user",     // 获取当前用户信息不需要认证
        "/api/auth/clear-credential", // 清除凭证不需要认证
        "/api/test/risk-control",     // 测试风控API不需要认证
        "/captcha",                   // 风控验证页面不需要认证
        "/api/captcha/info",          // 获取验证码信息不需要认证（风控验证页面使用）
        "/api/captcha/submit",        // 提交验证码结果不需要认证（风控验证页面使用）
        "/api/ws",                    // WebSocket使用协议头认证，不使用Authorization头
    ];

    let current_config = crate::config::reload_config();
    let token = current_config.auth_token.as_deref().unwrap_or("");

    if (path.starts_with("/api/logs/stream")
        || path.starts_with("/api/videos/live")
        || path.starts_with("/api/video-sources/live")
        || path.starts_with("/api/queue/live"))
        && request
            .uri()
            .query()
            .and_then(|query| serde_urlencoded::from_str::<HashMap<String, String>>(query).ok())
            .and_then(|params| params.get("token").cloned())
            .is_some_and(|query_token| query_token == token)
    {
        return Ok(next.run(request).await);
    }

    // 检查标准的Authorization头
    if headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s == token)
    {
        return Ok(next.run(request).await);
    }

    // 检查WebSocket协议头（用于WebSocket认证）
    if let Some(protocol) = headers.get("Sec-WebSocket-Protocol") {
        tracing::debug!("WebSocket协议头: {:?}", protocol);
        if let Ok(protocol_str) = protocol.to_str() {
            tracing::debug!("协议字符串: {}", protocol_str);
            if let Ok(decoded) = BASE64_URL_SAFE_NO_PAD.decode(protocol_str) {
                tracing::debug!("解码结果: {:?}, 期望: {:?}", decoded, token.as_bytes());
                if decoded == token.as_bytes() {
                    tracing::debug!("WebSocket认证成功");
                    return Ok(next.run(request).await);
                }
            }
        }
    }

    let needs_auth = path.starts_with("/api/") && !excluded_paths.iter().any(|&excluded| path.starts_with(excluded));

    if needs_auth {
        return Ok(ApiResponse::unauthorized(()).into_response());
    }
    Ok(next.run(request).await)
}

pub(super) struct OpenAPIAuth;

impl Modify for OpenAPIAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(schema) = openapi.components.as_mut() {
            schema.add_security_scheme(
                "Token",
                SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                    "Authorization",
                    "与配置文件中的 auth_token 相同",
                ))),
            );
        }
    }
}
