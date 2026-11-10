use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bilibili::BiliClient;

/// v_voucher风控验证处理模块
///
/// 当API返回v_voucher时，需要通过验证码验证来获取gaia_vtoken，
/// 然后用gaia_vtoken重新请求原API以绕过风控。
pub struct RiskControl<'a> {
    client: &'a BiliClient,
    v_voucher: String,
}

/// captcha验证码信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptchaInfo {
    /// 验证码类型，可能为 "geetest"（极验 v3）或 "geetest4"（极验 v4）
    #[serde(rename = "type")]
    pub captcha_type: String,
    /// 验证码token，用于后续验证
    pub token: String,
    /// 极验 v3 验证信息
    #[serde(default)]
    pub geetest: Option<GeetestInfo>,
    /// 极验 v4 验证信息（B 站若把 v4 参数放进 geetest4 对象）
    #[serde(default)]
    pub geetest4: Option<GeetestInfo>,
}

impl CaptchaInfo {
    /// 是否为极验 v4 验证码
    pub fn is_geetest4(&self) -> bool {
        self.captcha_type.contains("geetest4")
    }

    /// 取极验 captchaId：v4 的 geetest4.captcha_id 优先，其次 v3 的 captcha_id，最后回退 v3 的 gt
    pub fn geetest_captcha_id(&self) -> Option<&str> {
        self.geetest4
            .as_ref()
            .and_then(|info| info.captcha_id.as_deref())
            .or_else(|| self.geetest.as_ref().and_then(|info| info.captcha_id.as_deref()))
            .or_else(|| {
                self.geetest
                    .as_ref()
                    .map(|info| info.gt.as_str())
                    .filter(|gt| !gt.is_empty())
            })
    }
}

/// 极验验证信息（v3 的 gt/challenge，或 v4 的 captcha_id）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeetestInfo {
    /// 极验ID（v3 必填；v4 响应可能不返回）
    #[serde(default)]
    pub gt: String,
    /// 极验 challenge（v3 必填；v4 响应可能为空）
    #[serde(default)]
    pub challenge: String,
    /// 极验 v4 的 captchaId（部分响应会单独给出）
    #[serde(default)]
    pub captcha_id: Option<String>,
}

/// 验证结果
///
/// 极验 v3 使用 challenge/token/validate/seccode；
/// 极验 v4 使用 token/lot_number/captcha_output/pass_token/gen_time（challenge 仅作为上下文透传）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CaptchaResult {
    /// 验证码challenge
    #[serde(default)]
    pub challenge: String,
    /// 验证码token
    #[serde(default)]
    pub token: String,
    /// 验证结果validate（v3）
    #[serde(default)]
    pub validate: String,
    /// 验证结果seccode（v3）
    #[serde(default)]
    pub seccode: String,
    /// 极验 v4 验证结果：lot_number
    #[serde(default)]
    pub lot_number: Option<String>,
    /// 极验 v4 验证结果：captcha_output
    #[serde(default)]
    pub captcha_output: Option<String>,
    /// 极验 v4 验证结果：pass_token
    #[serde(default)]
    pub pass_token: Option<String>,
    /// 极验 v4 验证结果：gen_time
    #[serde(default)]
    pub gen_time: Option<String>,
}

impl CaptchaResult {
    /// 构造极验 v3 的验证结果（自动识别/三方打码服务使用）
    pub fn new_v3(challenge: String, token: String, validate: String, seccode: String) -> Self {
        Self {
            challenge,
            token,
            validate,
            seccode,
            ..Default::default()
        }
    }

    /// 是否为极验 v4 的验证结果
    pub fn is_geetest4(&self) -> bool {
        self.lot_number.is_some()
    }
}

/// 验证响应
#[derive(Debug, Deserialize)]
pub struct ValidateResponse {
    /// 验证是否成功，1表示成功（v4 响应可能不返回该字段）
    #[serde(default)]
    pub is_valid: i32,
    /// gaia_vtoken，用于恢复正常访问
    #[serde(default)]
    pub grisk_id: String,
}

/// 构造提交给 B 站 validate 接口的表单
///
/// - 极验 v4：提交 token/lot_number/captcha_output/pass_token/gen_time（另带 challenge 上下文），
///   不再提交 v3 的 validate/seccode；
/// - 极验 v3：保持原有 challenge/token/validate/seccode；
/// - csrf 存在时追加。
pub fn build_validate_form(captcha_result: &CaptchaResult, csrf: Option<&str>) -> Vec<(String, String)> {
    let mut form_data = if captcha_result.is_geetest4() {
        vec![
            ("token".to_string(), captcha_result.token.clone()),
            (
                "lot_number".to_string(),
                captcha_result.lot_number.clone().unwrap_or_default(),
            ),
            (
                "captcha_output".to_string(),
                captcha_result.captcha_output.clone().unwrap_or_default(),
            ),
            (
                "pass_token".to_string(),
                captcha_result.pass_token.clone().unwrap_or_default(),
            ),
            (
                "gen_time".to_string(),
                captcha_result.gen_time.clone().unwrap_or_default(),
            ),
            ("challenge".to_string(), captcha_result.challenge.clone()),
        ]
    } else {
        vec![
            ("challenge".to_string(), captcha_result.challenge.clone()),
            ("token".to_string(), captcha_result.token.clone()),
            ("validate".to_string(), captcha_result.validate.clone()),
            ("seccode".to_string(), captcha_result.seccode.clone()),
        ]
    };
    if let Some(csrf) = csrf {
        form_data.push(("csrf".to_string(), csrf.to_string()));
    }
    form_data
}

impl<'a> RiskControl<'a> {
    /// 创建新的风控处理实例
    pub fn new(client: &'a BiliClient, v_voucher: String) -> Self {
        Self { client, v_voucher }
    }

    /// 第一步：使用v_voucher申请captcha验证码
    ///
    /// 调用 https://api.bilibili.com/x/gaia-vgate/v1/register
    /// 参数：v_voucher, csrf（可选）
    /// 返回：token, challenge等验证码信息
    pub async fn register(&self) -> Result<CaptchaInfo> {
        tracing::info!("开始申请风控验证码，v_voucher: {}", self.v_voucher);

        let mut form_data = vec![("v_voucher", self.v_voucher.clone())];

        // 如果已登录，添加csrf token
        if let Some(csrf) = self.client.get_csrf_token() {
            form_data.push(("csrf", csrf));
        }

        let response = self
            .client
            .request(
                reqwest::Method::POST,
                "https://api.bilibili.com/x/gaia-vgate/v1/register",
            )
            .await
            .form(&form_data)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        // 检查响应
        if let Some(code) = response["code"].as_i64() {
            if code != 0 {
                let message = response["message"].as_str().unwrap_or("未知错误");
                anyhow::bail!("申请验证码失败: code={}, message={}", code, message);
            }
        }

        // 解析data字段
        let data = response["data"].clone();
        // 记录 register 原始 data，方便线上排查极验版本与字段变化（v3/v4）
        tracing::debug!("B站 register 接口原始 data: {}", data);
        let captcha_info: CaptchaInfo = serde_json::from_value(data)?;

        // 检查是否支持captcha验证（v3 的 geetest 或 v4 的 geetest4 至少有一个）
        if captcha_info.geetest.is_none() && captcha_info.geetest4.is_none() {
            anyhow::bail!("该风控无法通过captcha解除，geetest/geetest4 字段均为null");
        }

        tracing::info!(
            "成功获取验证码信息，类型: {}, token: {}, captcha_id: {:?}",
            captcha_info.captcha_type,
            captcha_info.token,
            captcha_info.geetest_captcha_id()
        );

        Ok(captcha_info)
    }

    /// 第二步：验证captcha并获取gaia_vtoken
    ///
    /// 调用 https://api.bilibili.com/x/gaia-vgate/v1/validate
    /// 参数：challenge, token, validate, seccode, csrf
    /// 返回：grisk_id (即gaia_vtoken)
    pub async fn validate(&self, captcha_result: CaptchaResult) -> Result<String> {
        tracing::info!("开始验证captcha，获取gaia_vtoken");
        tracing::debug!(
            "收到验证结果: 类型={}, token={}, challenge={}",
            if captcha_result.is_geetest4() {
                "geetest4"
            } else {
                "geetest"
            },
            captcha_result.token,
            captcha_result.challenge
        );

        // 构造提交表单：v4 提交四件套，v3 保持原字段；csrf 有则追加
        let csrf = self.client.get_csrf_token();
        let form_data = build_validate_form(&captcha_result, csrf.as_deref());

        let response = self
            .client
            .request(
                reqwest::Method::POST,
                "https://api.bilibili.com/x/gaia-vgate/v1/validate",
            )
            .await
            .form(&form_data)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        tracing::info!("B站验证API响应: {}", response);

        // 检查响应
        if let Some(code) = response["code"].as_i64() {
            if code != 0 {
                let message = response["message"].as_str().unwrap_or("未知错误");
                anyhow::bail!("验证失败: code={}, message={}", code, message);
            }
        }

        // 解析data字段
        let data = response["data"].clone();
        let validate_response: ValidateResponse = serde_json::from_value(data)?;

        if validate_response.is_valid != 1 {
            if validate_response.grisk_id.is_empty() {
                anyhow::bail!("验证未通过，is_valid={}", validate_response.is_valid);
            }
            // 极验 v4 的 validate 响应偶尔不带 is_valid，只要返回了非空 grisk_id 仍视为成功
            tracing::warn!(
                "验证接口 is_valid={}，但返回了非空 grisk_id，按成功处理",
                validate_response.is_valid
            );
        }

        tracing::info!("验证成功，获取到gaia_vtoken: {}", validate_response.grisk_id);
        Ok(validate_response.grisk_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_captcha_info(value: serde_json::Value) -> CaptchaInfo {
        serde_json::from_value(value).expect("解析 CaptchaInfo 失败")
    }

    #[test]
    fn parse_geetest4_captcha_info_from_register_data() {
        // B 站 v4 响应仍带 geetest 对象，gt 即 v4 的 captchaId，challenge 可能为空
        let info = parse_captcha_info(json!({
            "type": "geetest4",
            "token": "81e230b0a3264d0f8e0a92038e53e653",
            "geetest": { "gt": "0123456789abcdef0123456789abcdef", "challenge": "" }
        }));
        assert!(info.is_geetest4());
        assert_eq!(info.geetest_captcha_id(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(
            info.geetest.as_ref().map(|geetest| geetest.challenge.as_str()),
            Some("")
        );
    }

    #[test]
    fn parse_geetest4_captcha_info_from_geetest4_object() {
        let info = parse_captcha_info(json!({
            "type": "geetest4",
            "token": "token-v4",
            "geetest": null,
            "geetest4": { "captcha_id": "v4-only-captcha-id" }
        }));
        assert!(info.is_geetest4());
        assert_eq!(info.geetest_captcha_id(), Some("v4-only-captcha-id"));
    }

    #[test]
    fn geetest_captcha_id_prefers_v4_captcha_id() {
        let info = parse_captcha_info(json!({
            "type": "geetest4",
            "token": "token",
            "geetest": { "gt": "gt-fallback", "challenge": "challenge" },
            "geetest4": { "captcha_id": "v4-captcha-id" }
        }));
        assert_eq!(info.geetest_captcha_id(), Some("v4-captcha-id"));

        let v3 = parse_captcha_info(json!({
            "type": "geetest",
            "token": "token",
            "geetest": { "gt": "gt-only", "challenge": "challenge" }
        }));
        assert!(!v3.is_geetest4());
        assert_eq!(v3.geetest_captcha_id(), Some("gt-only"));

        let v3_with_captcha_id = parse_captcha_info(json!({
            "type": "geetest",
            "token": "token",
            "geetest": { "gt": "gt-value", "challenge": "challenge", "captcha_id": "v3-captcha-id" }
        }));
        assert_eq!(v3_with_captcha_id.geetest_captcha_id(), Some("v3-captcha-id"));
    }

    #[test]
    fn build_validate_form_uses_v4_fields_with_csrf() {
        let result = CaptchaResult {
            token: "token-v4".to_string(),
            challenge: "challenge-v4".to_string(),
            lot_number: Some("lot-1".to_string()),
            captcha_output: Some("output-1".to_string()),
            pass_token: Some("pass-1".to_string()),
            gen_time: Some("1700000000".to_string()),
            ..Default::default()
        };
        let form = build_validate_form(&result, Some("csrf-1"));
        let keys: Vec<&str> = form.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "token",
                "lot_number",
                "captcha_output",
                "pass_token",
                "gen_time",
                "challenge",
                "csrf"
            ]
        );
        assert!(!keys.contains(&"validate"));
        assert!(!keys.contains(&"seccode"));
        assert_eq!(
            form.iter()
                .find(|(key, _)| key == "lot_number")
                .map(|(_, value)| value.as_str()),
            Some("lot-1")
        );
        assert_eq!(
            form.iter()
                .find(|(key, _)| key == "csrf")
                .map(|(_, value)| value.as_str()),
            Some("csrf-1")
        );
    }

    #[test]
    fn build_validate_form_v4_without_csrf() {
        let result = CaptchaResult {
            token: "token-v4".to_string(),
            lot_number: Some("lot-2".to_string()),
            captcha_output: Some("output-2".to_string()),
            pass_token: Some("pass-2".to_string()),
            gen_time: Some("1700000001".to_string()),
            ..Default::default()
        };
        let form = build_validate_form(&result, None);
        let keys: Vec<&str> = form.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "token",
                "lot_number",
                "captcha_output",
                "pass_token",
                "gen_time",
                "challenge"
            ]
        );
    }

    #[test]
    fn build_validate_form_keeps_v3_fields() {
        let result = CaptchaResult::new_v3(
            "challenge-v3".to_string(),
            "token-v3".to_string(),
            "validate-v3".to_string(),
            "validate-v3|jordan".to_string(),
        );
        let form = build_validate_form(&result, None);
        let keys: Vec<&str> = form.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["challenge", "token", "validate", "seccode"]);
        assert_eq!(
            form.iter()
                .find(|(key, _)| key == "seccode")
                .map(|(_, value)| value.as_str()),
            Some("validate-v3|jordan")
        );
    }
}
