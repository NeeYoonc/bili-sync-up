use axum::{
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};

use super::{CaptchaResult, VERIFICATION_COORDINATOR};

/// 验证结果提交参数
///
/// 极验 v3 提交 `geetest_challenge/geetest_validate/geetest_seccode`，
/// 极验 v4 提交 `lot_number/captcha_output/pass_token/gen_time`，两种组合都接受。
#[derive(Debug, Deserialize)]
pub struct SubmitCaptchaParams {
    /// 极验 v3：challenge（可选，缺失时用登记时保存的 challenge 兜底）
    #[serde(default)]
    pub geetest_challenge: Option<String>,
    /// 极验 v3：validate
    #[serde(default)]
    pub geetest_validate: Option<String>,
    /// 极验 v3：seccode
    #[serde(default)]
    pub geetest_seccode: Option<String>,
    /// 极验 v4：lot_number
    #[serde(default)]
    pub lot_number: Option<String>,
    /// 极验 v4：captcha_output
    #[serde(default)]
    pub captcha_output: Option<String>,
    /// 极验 v4：pass_token
    #[serde(default)]
    pub pass_token: Option<String>,
    /// 极验 v4：gen_time
    #[serde(default)]
    pub gen_time: Option<String>,
}

/// API响应
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub message: String,
}

impl ApiResponse {
    pub fn success<T: Serialize>(data: T) -> Self {
        Self {
            success: true,
            data: Some(serde_json::to_value(data).unwrap()),
            message: "操作成功".to_string(),
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            message,
        }
    }
}

/// 验证码页面
pub async fn serve_captcha_page() -> impl IntoResponse {
    Html(include_str!("captcha_page.html"))
}

/// 获取验证码信息API
pub async fn get_captcha_info() -> impl IntoResponse {
    if let Some(captcha_info) = VERIFICATION_COORDINATOR.get_captcha_info().await {
        Json(ApiResponse::success(captcha_info))
    } else {
        Json(ApiResponse::error("验证码信息未找到".to_string()))
    }
}

/// 提交验证结果API
pub async fn submit_captcha_result(Json(params): Json<SubmitCaptchaParams>) -> impl IntoResponse {
    tracing::info!("收到验证码结果提交");

    // 获取验证码信息以构造完整的结果
    let captcha_info = match VERIFICATION_COORDINATOR.get_captcha_info().await {
        Some(info) => info,
        None => {
            return Json(ApiResponse::error("验证码信息未找到".to_string()));
        }
    };

    // 根据当前验证码类型，检查提交的字段组合是否完整
    let v4_complete = params.lot_number.is_some()
        && params.captcha_output.is_some()
        && params.pass_token.is_some()
        && params.gen_time.is_some();
    let v3_complete = params.geetest_validate.is_some() && params.geetest_seccode.is_some();
    let is_v4 = captcha_info.is_geetest4();
    if is_v4 && !v4_complete {
        return Json(ApiResponse::error(
            "提交参数不完整：当前为极验 v4 验证，需要 lot_number/captcha_output/pass_token/gen_time".to_string(),
        ));
    }
    if !is_v4 && !v3_complete {
        return Json(ApiResponse::error(
            "提交参数不完整：当前为极验 v3 验证，需要 geetest_validate/geetest_seccode".to_string(),
        ));
    }

    // v3 的 challenge 允许由前端回传，缺失时用登记时保存的 challenge 兜底
    let challenge = params
        .geetest_challenge
        .clone()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            captcha_info
                .geetest
                .as_ref()
                .map(|geetest| geetest.challenge.clone())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default();

    // 构造验证结果
    let captcha_result = if is_v4 {
        CaptchaResult {
            challenge,
            token: captcha_info.token.clone(),
            lot_number: params.lot_number.clone(),
            captcha_output: params.captcha_output.clone(),
            pass_token: params.pass_token.clone(),
            gen_time: params.gen_time.clone(),
            ..Default::default()
        }
    } else {
        CaptchaResult::new_v3(
            challenge,
            captcha_info.token.clone(),
            params.geetest_validate.clone().unwrap_or_default(),
            params.geetest_seccode.clone().unwrap_or_default(),
        )
    };

    // 提交结果到协调器
    match VERIFICATION_COORDINATOR.submit_captcha_result(captcha_result).await {
        Ok(_) => {
            tracing::info!("验证码结果已成功提交");
            Json(ApiResponse::success("验证完成，窗口将自动关闭"))
        }
        Err(e) => {
            tracing::error!("验证码结果提交失败: {}", e);
            Json(ApiResponse::error(format!("提交失败: {}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_params_parse_v3_only() {
        let params: SubmitCaptchaParams = serde_json::from_value(serde_json::json!({
            "geetest_challenge": "challenge-v3",
            "geetest_validate": "validate-v3",
            "geetest_seccode": "validate-v3|jordan"
        }))
        .expect("v3 提交参数解析失败");
        assert_eq!(params.geetest_validate.as_deref(), Some("validate-v3"));
        assert!(params.lot_number.is_none());
        assert!(params.captcha_output.is_none());
    }

    #[test]
    fn submit_params_parse_v4_only() {
        let params: SubmitCaptchaParams = serde_json::from_value(serde_json::json!({
            "lot_number": "lot-1",
            "captcha_output": "output-1",
            "pass_token": "pass-1",
            "gen_time": "1700000000"
        }))
        .expect("v4 提交参数解析失败");
        assert_eq!(params.lot_number.as_deref(), Some("lot-1"));
        assert_eq!(params.gen_time.as_deref(), Some("1700000000"));
        assert!(params.geetest_validate.is_none());
        assert!(params.geetest_seccode.is_none());
    }
}
