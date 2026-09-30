//! 弹幕增量更新的调度决策函数（纯函数，易测试）。

use chrono::{DateTime, Duration, Utc};

use crate::config::DanmakuUpdatePolicy;

/// 弹幕同步阶段（与数据库 `page.danmaku_sync_generation` 字段一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Initial = 0,
    Fresh = 1,
    Mature = 2,
    Cold = 3,
    Frozen = 4,
}

impl Stage {
    pub fn from_generation(generation: u32) -> Self {
        match generation {
            0 => Stage::Initial,
            1 => Stage::Fresh,
            2 => Stage::Mature,
            3 => Stage::Cold,
            _ => Stage::Frozen,
        }
    }

    pub fn as_generation(self) -> u32 {
        self as u32
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Initial => "未同步",
            Stage::Fresh => "新鲜期",
            Stage::Mature => "成熟期",
            Stage::Cold => "老化期",
            Stage::Frozen => "已冻结",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Skip,
    Sync { next_stage: Stage },
}

pub fn stage_for_age(
    policy: &DanmakuUpdatePolicy,
    pubtime: DateTime<Utc>,
    now: DateTime<Utc>,
    allow_freeze: bool,
) -> Stage {
    let age = now.signed_duration_since(pubtime).max(Duration::zero());
    let fresh_end = Duration::days(policy.fresh_days as i64);
    let mature_end = Duration::days(policy.mature_days as i64);
    let cold_end = Duration::days(policy.cold_days as i64);

    if allow_freeze && age >= cold_end {
        Stage::Frozen
    } else if age < fresh_end {
        Stage::Fresh
    } else if age < mature_end {
        Stage::Mature
    } else {
        Stage::Cold
    }
}

pub fn should_sync_danmaku(
    policy: &DanmakuUpdatePolicy,
    pubtime: DateTime<Utc>,
    last_synced: Option<DateTime<Utc>>,
    generation: u32,
    now: DateTime<Utc>,
) -> Decision {
    if !policy.enabled {
        return Decision::Skip;
    }

    let current_stage = Stage::from_generation(generation);
    if current_stage == Stage::Frozen {
        return Decision::Skip;
    }

    let target_stage = stage_for_age(policy, pubtime, now, true);
    let interval = stage_interval(policy, target_stage);

    if target_stage == Stage::Frozen {
        // 冷冻期只补最后一次同步，这一步同样要等满老化期间隔：
        // 否则刚刷新过的分页（阶段仍记为老化期）下一轮会被立刻再刷一次，
        // 而且写回的阶段永远不是“已冻结”，就会一直循环刷新下去。
        return match last_synced {
            None => Decision::Sync {
                next_stage: Stage::Frozen,
            },
            Some(last_synced_at) if elapsed_since(last_synced_at, now) >= interval => Decision::Sync {
                next_stage: Stage::Frozen,
            },
            Some(_) => Decision::Skip,
        };
    }

    match last_synced {
        None => Decision::Sync {
            next_stage: target_stage,
        },
        Some(last_synced_at) => {
            if elapsed_since(last_synced_at, now) >= interval
                || target_stage.as_generation() > current_stage.as_generation()
            {
                Decision::Sync {
                    next_stage: target_stage,
                }
            } else {
                Decision::Skip
            }
        }
    }
}

fn elapsed_since(last_synced_at: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    now.signed_duration_since(last_synced_at).max(Duration::zero())
}

fn stage_interval(policy: &DanmakuUpdatePolicy, stage: Stage) -> Duration {
    match stage {
        Stage::Fresh => Duration::hours(policy.fresh_interval_hours as i64),
        Stage::Mature => Duration::days(policy.mature_interval_days as i64),
        Stage::Cold | Stage::Frozen => Duration::days(policy.cold_interval_days as i64),
        Stage::Initial => Duration::zero(),
    }
}

/// 刷新完成后写回数据库的阶段。
///
/// 必须和 [`should_sync_danmaku`] 得到的目标阶段一致（冷冻判定同样生效），
/// 否则超龄视频每轮都会被判成“还差最后一次补刷”，刷新完又写回老化期，
/// 于是无限重复刷新。
pub fn resolve_sync_stage(
    policy: &DanmakuUpdatePolicy,
    pubtime: DateTime<Utc>,
    now: DateTime<Utc>,
    planned: Option<Stage>,
) -> Stage {
    planned.unwrap_or_else(|| stage_for_age(policy, pubtime, now, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DanmakuUpdatePolicy {
        DanmakuUpdatePolicy {
            enabled: true,
            fresh_days: 3,
            fresh_interval_hours: 6,
            mature_days: 30,
            mature_interval_days: 3,
            cold_days: 180,
            cold_interval_days: 30,
        }
    }

    fn t(days: i64, hours: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap() + Duration::days(days) + Duration::hours(hours)
    }

    #[test]
    fn disabled_always_skip() {
        let mut policy = policy();
        policy.enabled = false;
        assert_eq!(should_sync_danmaku(&policy, t(0, 0), None, 0, t(10, 0)), Decision::Skip);
    }

    #[test]
    fn first_sync_triggers() {
        assert_eq!(
            should_sync_danmaku(&policy(), t(0, 0), None, 0, t(0, 1)),
            Decision::Sync {
                next_stage: Stage::Fresh
            }
        );
    }

    #[test]
    fn stage_transition_triggers_immediately() {
        assert_eq!(
            should_sync_danmaku(&policy(), t(0, 0), Some(t(0, 2)), Stage::Fresh.as_generation(), t(5, 0)),
            Decision::Sync {
                next_stage: Stage::Mature
            }
        );
    }

    #[test]
    fn frozen_always_skips() {
        assert_eq!(
            should_sync_danmaku(
                &policy(),
                t(0, 0),
                Some(t(200, 0)),
                Stage::Frozen.as_generation(),
                t(300, 0)
            ),
            Decision::Skip
        );
    }

    #[test]
    fn frozen_final_sync_waits_for_cold_interval() {
        let policy = policy();
        // 1 小时前刚刷新过：不再立刻补刷
        assert_eq!(
            should_sync_danmaku(&policy, t(0, 0), Some(t(200, 0)), Stage::Cold.as_generation(), t(200, 1)),
            Decision::Skip
        );
        // 距上次同步已超过老化期间隔：补最后一次并冻结
        assert_eq!(
            should_sync_danmaku(&policy, t(0, 0), Some(t(100, 0)), Stage::Cold.as_generation(), t(200, 0)),
            Decision::Sync {
                next_stage: Stage::Frozen
            }
        );
    }

    #[test]
    fn resolve_sync_stage_marks_frozen_for_old_video() {
        let policy = policy();
        assert_eq!(resolve_sync_stage(&policy, t(0, 0), t(200, 0), None), Stage::Frozen);
        assert_eq!(resolve_sync_stage(&policy, t(0, 0), t(2, 0), None), Stage::Fresh);
        assert_eq!(resolve_sync_stage(&policy, t(0, 0), t(10, 0), None), Stage::Mature);
        assert_eq!(resolve_sync_stage(&policy, t(0, 0), t(60, 0), None), Stage::Cold);
        assert_eq!(
            resolve_sync_stage(&policy, t(0, 0), t(200, 0), Some(Stage::Mature)),
            Stage::Mature
        );
    }

    /// 复现 issue #222：刷新时没有携带调度计划，写回的阶段必须自己算出「已冻结」，
    /// 否则下一轮又会判定成待刷新，无限重复。
    #[test]
    fn refresh_written_stage_stops_frozen_loop() {
        let policy = policy();
        let pubtime = t(0, 0);
        let now = t(200, 0);
        let recorded = resolve_sync_stage(&policy, pubtime, now, None);
        assert_eq!(recorded, Stage::Frozen);
        assert_eq!(
            should_sync_danmaku(&policy, pubtime, Some(now), recorded.as_generation(), now),
            Decision::Skip
        );
        assert_eq!(
            should_sync_danmaku(
                &policy,
                pubtime,
                Some(now),
                recorded.as_generation(),
                now + Duration::days(10)
            ),
            Decision::Skip
        );
    }

    #[test]
    fn stage_label_returns_chinese_text() {
        assert_eq!(Stage::Initial.label(), "未同步");
        assert_eq!(Stage::Fresh.label(), "新鲜期");
        assert_eq!(Stage::Mature.label(), "成熟期");
        assert_eq!(Stage::Cold.label(), "老化期");
        assert_eq!(Stage::Frozen.label(), "已冻结");
    }
}
