use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use once_cell::sync::Lazy;
use tokio::sync::{oneshot, Mutex, Notify};

use super::{CaptchaInfo, CaptchaResult, CaptchaSolver};
use crate::config::RiskControlConfig;

/// 风控验证默认超时时间（秒）。
///
/// 正常路径由调用方传入风控配置里的 `risk_config.timeout`，这个常量只作为无配置场景
/// （例如单测）的兜底，避免再次出现写死的超时。
#[allow(dead_code, reason = "保留给无配置场景与单测兜底使用")]
pub const DEFAULT_VERIFICATION_TIMEOUT_SECS: u64 = 300;

/// 验证请求类型
#[derive(Debug)]
pub enum VerificationRequest {
    /// 开始新的验证流程
    StartNew(CaptchaInfo),
    /// 等待现有验证完成
    WaitForExisting,
    /// 使用缓存的token
    UseCache(String),
}

/// 验证状态
#[derive(Debug)]
enum VerificationState {
    /// 空闲状态
    Idle,
    /// 等待用户验证
    WaitingForUser {
        captcha_info: CaptchaInfo,
        result_sender: Option<oneshot::Sender<CaptchaResult>>,
    },
    /// 验证完成，token可用
    Completed { gaia_vtoken: String, expires_at: Instant },
}

/// 全局验证协调器
pub struct VerificationCoordinator {
    state: Arc<Mutex<VerificationState>>,
    notify: Arc<Notify>,
}

impl VerificationCoordinator {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(VerificationState::Idle)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// 请求验证，返回应该执行的操作
    pub async fn request_verification(
        &self,
        v_voucher: String,
        captcha_info: CaptchaInfo,
    ) -> Result<VerificationRequest> {
        let mut state = self.state.lock().await;

        match &*state {
            VerificationState::Idle => {
                tracing::info!("启动新的验证流程，v_voucher: {}", v_voucher);
                // 设置为等待状态
                *state = VerificationState::WaitingForUser {
                    captcha_info: captcha_info.clone(),
                    result_sender: None,
                };
                Ok(VerificationRequest::StartNew(captcha_info))
            }
            VerificationState::WaitingForUser { .. } => {
                tracing::info!("检测到正在进行的验证，等待完成...");
                Ok(VerificationRequest::WaitForExisting)
            }
            VerificationState::Completed {
                gaia_vtoken,
                expires_at,
            } => {
                if expires_at > &Instant::now() {
                    tracing::info!("使用缓存的gaia_vtoken");
                    Ok(VerificationRequest::UseCache(gaia_vtoken.clone()))
                } else {
                    tracing::info!("缓存的token已过期，启动新的验证流程");
                    *state = VerificationState::WaitingForUser {
                        captcha_info: captcha_info.clone(),
                        result_sender: None,
                    };
                    Ok(VerificationRequest::StartNew(captcha_info))
                }
            }
        }
    }

    /// 获取当前验证码信息（用于API）
    pub async fn get_captcha_info(&self) -> Option<CaptchaInfo> {
        let state = self.state.lock().await;
        match &*state {
            VerificationState::WaitingForUser { captcha_info, .. } => Some(captcha_info.clone()),
            _ => None,
        }
    }

    /// 提交验证结果（用于API）
    pub async fn submit_captcha_result(&self, result: CaptchaResult) -> Result<()> {
        let mut state = self.state.lock().await;

        match std::mem::replace(&mut *state, VerificationState::Idle) {
            VerificationState::WaitingForUser { result_sender, .. } => {
                if let Some(sender) = result_sender {
                    let _ = sender.send(result);
                }
                tracing::info!("验证结果已提交，等待处理...");
                Ok(())
            }
            old_state => {
                *state = old_state;
                anyhow::bail!("当前没有等待验证的状态");
            }
        }
    }

    /// 等待验证完成，返回验证结果
    ///
    /// `timeout` 由调用方传入（通常取风控配置里的 `risk_config.timeout`）。超时或验证通道
    /// 关闭时会先把状态重置为 `Idle`，再唤醒所有 `wait_for_completion` 等待者，避免它们
    /// 因为状态已被重置而永久阻塞。
    pub async fn wait_for_captcha_result(&self, timeout: Duration) -> Result<CaptchaResult> {
        // 首先设置result_sender
        let receiver = {
            let mut state = self.state.lock().await;
            match &mut *state {
                VerificationState::WaitingForUser { result_sender, .. } => {
                    let (tx, rx) = oneshot::channel();
                    *result_sender = Some(tx);
                    rx
                }
                _ => {
                    anyhow::bail!("验证状态异常");
                }
            }
        };

        // 等待验证完成（带超时）
        let result = tokio::time::timeout(timeout, receiver).await;

        match result {
            Ok(Ok(captcha_result)) => {
                tracing::info!("收到验证结果");
                Ok(captcha_result)
            }
            Ok(Err(_)) => {
                // 验证通道已关闭：重置状态并唤醒所有等待者，避免它们永久阻塞。
                self.reset_to_idle_and_wake().await;
                anyhow::bail!("验证通道已关闭");
            }
            Err(_) => {
                // 超时：重置状态并唤醒所有 WaitForExisting 等待者，避免它们永久挂住。
                self.reset_to_idle_and_wake().await;
                anyhow::bail!(
                    "风控验证未在 {} 秒内完成，本次任务按失败处理（可重试）",
                    timeout.as_secs()
                );
            }
        }
    }

    /// 仅当前状态仍是「等待用户验证」时重置为 `Idle`，然后唤醒所有等待者。
    ///
    /// 已完成的 token 不会被这里覆盖。
    async fn reset_to_idle_and_wake(&self) {
        {
            let mut state = self.state.lock().await;
            if matches!(*state, VerificationState::WaitingForUser { .. }) {
                *state = VerificationState::Idle;
            }
        }
        self.notify.notify_waiters();
    }

    /// 当前是否处于空闲状态（发起验证的任务已超时/放弃）。
    async fn is_idle(&self) -> bool {
        matches!(*self.state.lock().await, VerificationState::Idle)
    }

    /// 保存验证成功的token
    pub async fn save_token(&self, gaia_vtoken: String) {
        let mut state = self.state.lock().await;
        *state = VerificationState::Completed {
            gaia_vtoken,
            expires_at: Instant::now() + Duration::from_secs(3600), // 1小时过期
        };

        // 通知所有等待的进程
        self.notify.notify_waiters();
        tracing::info!("gaia_vtoken已保存并通知等待进程");
    }

    /// 获取缓存的token
    pub async fn get_cached_token(&self) -> Option<String> {
        let state = self.state.lock().await;
        match &*state {
            VerificationState::Completed {
                gaia_vtoken,
                expires_at,
            } => {
                if expires_at > &Instant::now() {
                    Some(gaia_vtoken.clone())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 等待验证完成的通知
    ///
    /// `timeout` 由调用方传入（通常取风控配置里的 `risk_config.timeout`）。循环中会同时
    /// 检查缓存 token、协调器状态是否已被重置为 `Idle`（说明发起验证的任务已超时/放弃）
    /// 以及是否到达截止时间，因此不会再出现永久阻塞。
    pub async fn wait_for_completion(&self, timeout: Duration) -> Result<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // 检查是否已完成
            if let Some(token) = self.get_cached_token().await {
                return Ok(token);
            }

            // 发起验证的任务已超时/放弃（状态被重置为 Idle）→ 立即失败，不再永久等待
            if self.is_idle().await {
                anyhow::bail!("风控验证已被放弃或超时，本次任务按失败处理（可重试）");
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                // 超时：仅在仍等待用户时重置状态，避免覆盖已完成的 token
                self.reset_to_idle_and_wake().await;
                anyhow::bail!(
                    "风控验证未在 {} 秒内完成，本次任务按失败处理（可重试）",
                    timeout.as_secs()
                );
            }

            // 等待下一次通知，最多等到截止时间
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(deadline - now) => {}
            }
        }
    }

    /// 自动解决验证码
    pub async fn auto_solve_captcha(&self, config: &RiskControlConfig, page_url: &str) -> Result<CaptchaResult> {
        let captcha_info = self
            .get_captcha_info()
            .await
            .ok_or_else(|| anyhow::anyhow!("当前没有待验证的验证码"))?;

        // 检查是否有GeeTest信息
        let geetest_info = captcha_info
            .geetest
            .ok_or_else(|| anyhow::anyhow!("验证码信息中缺少GeeTest数据"))?;

        // 获取自动解决配置
        let auto_solve_config = config
            .auto_solve
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("自动解决功能未配置"))?;

        tracing::info!("开始自动解决验证码，服务: {}", auto_solve_config.service);

        // 创建验证码解决器
        let solver = CaptchaSolver::new(auto_solve_config.clone());

        // 解决验证码
        let result = solver
            .solve_geetest(&geetest_info, &captcha_info.token, page_url)
            .await?;

        tracing::info!("验证码自动解决成功");
        Ok(result)
    }
}

/// 全局验证协调器实例
pub static VERIFICATION_COORDINATOR: Lazy<VerificationCoordinator> = Lazy::new(VerificationCoordinator::new);

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn sample_captcha_info() -> CaptchaInfo {
        CaptchaInfo {
            captcha_type: "geetest".to_string(),
            token: "test-captcha-token".to_string(),
            geetest: None,
            geetest4: None,
        }
    }

    async fn start_verification(coordinator: &VerificationCoordinator) {
        let request = coordinator
            .request_verification("test-v-voucher".to_string(), sample_captcha_info())
            .await
            .expect("启动验证请求失败");
        assert!(matches!(request, VerificationRequest::StartNew(_)));
    }

    #[tokio::test]
    async fn wait_for_completion_returns_error_after_timeout() {
        let coordinator = VerificationCoordinator::new();
        start_verification(&coordinator).await;

        let err = coordinator
            .wait_for_completion(Duration::from_millis(50))
            .await
            .expect_err("超时后应返回错误，而不是永久挂住");
        assert!(err.to_string().contains("风控验证未在"));
        // 超时后状态应回到 Idle
        assert!(coordinator.get_captcha_info().await.is_none());
    }

    #[tokio::test]
    async fn captcha_timeout_wakes_wait_for_completion_waiters() {
        let coordinator = Arc::new(VerificationCoordinator::new());
        start_verification(&coordinator).await;

        let waiter = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move { coordinator.wait_for_completion(Duration::from_secs(5)).await })
        };
        // 让等待者先进入等待，再让发起方超时
        tokio::time::sleep(Duration::from_millis(50)).await;

        let captcha_err = coordinator
            .wait_for_captcha_result(Duration::from_millis(100))
            .await
            .expect_err("验证码等待应超时返回错误");
        assert!(captcha_err.to_string().contains("风控验证未在"));

        let waiter_result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("发起方超时后，WaitForExisting 等待者应立即醒来")
            .expect("等待任务 panic");
        let waiter_err = waiter_result.expect_err("发起方超时后等待者不应返回成功");
        assert!(waiter_err.to_string().contains("风控验证已被放弃或超时"));
        assert!(coordinator.is_idle().await);
    }

    #[tokio::test]
    async fn wait_for_completion_returns_token_after_save() {
        let coordinator = VerificationCoordinator::new();
        start_verification(&coordinator).await;
        assert!(coordinator.get_cached_token().await.is_none());

        coordinator.save_token("test-gaia-vtoken".to_string()).await;
        let token = coordinator
            .wait_for_completion(Duration::from_millis(200))
            .await
            .expect("已保存 token 后应立即返回 Ok");
        assert_eq!(token, "test-gaia-vtoken");
    }

    #[tokio::test]
    async fn wait_for_completion_returns_error_when_state_reset_externally() {
        let coordinator = Arc::new(VerificationCoordinator::new());
        start_verification(&coordinator).await;

        let waiter = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move { coordinator.wait_for_completion(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        // 模拟发起验证的任务超时/放弃后把状态重置为 Idle
        {
            let mut state = coordinator.state.lock().await;
            *state = VerificationState::Idle;
        }
        coordinator.notify.notify_waiters();

        let waiter_result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("状态被置 Idle 后等待者应立即醒来")
            .expect("等待任务 panic");
        let waiter_err = waiter_result.expect_err("状态已被重置时不应返回成功");
        assert!(waiter_err.to_string().contains("风控验证已被放弃或超时"));
    }
}
