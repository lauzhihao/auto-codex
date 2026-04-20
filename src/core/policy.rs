#![allow(dead_code)]

use crate::core::state::{
    AccountRecord, CURRENT_ACCOUNT_MIN_FIVE_HOUR_PERCENT, LiveIdentity, State, UsageSnapshot,
};

pub fn choose_best_account<'a>(state: &'a State) -> Option<&'a AccountRecord> {
    let mut candidates: Vec<((i64, i64, i64, f64, i64, i64), &AccountRecord)> = state
        .accounts
        .iter()
        .filter(|account| account.is_subscription())
        .filter_map(|account| {
            let usage = state.usage_cache.get(&account.id)?;
            if usage.needs_relogin || usage.last_sync_error.is_some() {
                return None;
            }
            // 排除周额度 <= 5% 的账号
            if let Some(weekly) = usage.weekly_remaining_percent {
                if weekly <= 5 {
                    return None;
                }
            } else {
                return None;
            }
            Some((build_score(account, usage), account))
        })
        .collect();

    candidates.sort_by(|left, right| right.0.total_cmp(&left.0));
    candidates.first().map(|(_, account)| *account)
}

pub fn choose_current_account<'a>(
    state: &'a State,
    live: Option<&LiveIdentity>,
) -> Option<&'a AccountRecord> {
    let live = live?;
    let account = state
        .accounts
        .iter()
        .filter(|account| account.is_subscription())
        .find(|account| identity_matches(account, live))?;
    let usage = state.usage_cache.get(&account.id)?;
    is_current_account_usable(usage).then_some(account)
}

pub fn identity_matches(account: &AccountRecord, live: &LiveIdentity) -> bool {
    if live.scodex_account_id.as_deref() == Some(account.id.as_str()) {
        return true;
    }

    if account.email.eq_ignore_ascii_case(&live.email) {
        return true;
    }

    match (&account.account_id, &live.account_id) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub fn is_current_account_usable(usage: &UsageSnapshot) -> bool {
    if usage.needs_relogin || usage.last_sync_error.is_some() {
        return false;
    }

    let five_hour_ok = match usage.five_hour_remaining_percent {
        Some(value) => (value as f64) >= CURRENT_ACCOUNT_MIN_FIVE_HOUR_PERCENT,
        None => false,
    };

    let weekly_ok = match usage.weekly_remaining_percent {
        Some(value) => value > 5,
        None => false,
    };

    five_hour_ok && weekly_ok
}

fn build_score(account: &AccountRecord, usage: &UsageSnapshot) -> (i64, i64, i64, f64, i64, i64) {
    (
        quota_score(usage.five_hour_remaining_percent),
        quota_score(usage.weekly_remaining_percent),
        // 重置时间戳越早（数值越小）越优先；缺失视为无限远，排到最差
        parse_refresh_ts(&usage.weekly_refresh_at),
        usage.credits_balance.unwrap_or(-1.0),
        usage.last_synced_at.unwrap_or(0),
        account.updated_at,
    )
}

fn quota_score(value: Option<i64>) -> i64 {
    // 分档而非精确百分比：同档视为打平，让重置时间等次级键生效
    match value {
        None => -1,
        Some(p) if p >= 75 => 4,
        Some(p) if p >= 50 => 3,
        Some(p) if p >= 20 => 2,
        Some(_) => 1,
    }
}

fn parse_refresh_ts(value: &Option<String>) -> i64 {
    match value
        .as_deref()
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
    {
        Some(dt) => dt.timestamp(),
        None => i64::MAX,
    }
}

trait TotalCmpTuple {
    fn total_cmp(&self, other: &Self) -> std::cmp::Ordering;
}

impl TotalCmpTuple for (i64, i64, i64, f64, i64, i64) {
    fn total_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .cmp(&other.0)
            .then(self.1.cmp(&other.1))
            // 重置时间戳：升序（越早越优），所以在降序外壳里反向比较
            .then_with(|| other.2.cmp(&self.2))
            .then_with(|| self.3.total_cmp(&other.3))
            .then(self.4.cmp(&other.4))
            .then(self.5.cmp(&other.5))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{choose_best_account, choose_current_account, is_current_account_usable};
    use crate::core::state::{AccountRecord, AccountType, LiveIdentity, State, UsageSnapshot};

    #[test]
    fn keeps_current_account_when_threshold_is_met() {
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "current".into(),
                    email: "current@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "better".into(),
                    email: "better@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "current".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(25),
                        weekly_remaining_percent: Some(20),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "better".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(95),
                        weekly_remaining_percent: Some(90),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let current = choose_current_account(
            &state,
            Some(&LiveIdentity {
                email: "current@example.com".into(),
                account_id: None,
                scodex_account_id: None,
            }),
        );

        assert_eq!(current.map(|item| item.id.as_str()), Some("current"));
    }

    #[test]
    fn current_account_below_threshold_is_not_usable() {
        let usage = UsageSnapshot {
            five_hour_remaining_percent: Some(19),
            weekly_remaining_percent: Some(50),
            ..UsageSnapshot::default()
        };

        assert!(!is_current_account_usable(&usage));
    }

    #[test]
    fn best_account_ignores_api_accounts() {
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "api".into(),
                    account_type: AccountType::Api,
                    email: "56wxyz@openrouter".into(),
                    updated_at: 100,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "subscription".into(),
                    email: "subscription@example.com".into(),
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "api".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(100),
                        weekly_remaining_percent: Some(100),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "subscription".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(25),
                        weekly_remaining_percent: Some(20),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(best.map(|item| item.id.as_str()), Some("subscription"));
    }

    #[test]
    fn current_account_with_low_weekly_quota_is_not_usable() {
        let usage = UsageSnapshot {
            five_hour_remaining_percent: Some(50),
            weekly_remaining_percent: Some(5),
            ..UsageSnapshot::default()
        };

        assert!(!is_current_account_usable(&usage));
    }

    #[test]
    fn current_account_with_sync_error_is_not_usable() {
        let usage = UsageSnapshot {
            five_hour_remaining_percent: Some(80),
            last_sync_error: Some("quota api failed".into()),
            ..UsageSnapshot::default()
        };

        assert!(!is_current_account_usable(&usage));
    }

    #[test]
    fn best_account_prefers_five_hour_quota() {
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "weekly-heavy".into(),
                    email: "weekly@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "five-heavy".into(),
                    email: "five@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "weekly-heavy".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(5),
                        weekly_remaining_percent: Some(95),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "five-heavy".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(60),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(best.map(|item| item.id.as_str()), Some("five-heavy"));
    }

    #[test]
    fn best_account_ignores_sync_error_candidates() {
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "stale".into(),
                    email: "stale@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "healthy".into(),
                    email: "healthy@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "stale".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(100),
                        weekly_remaining_percent: Some(100),
                        last_sync_error: Some("quota api failed".into()),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "healthy".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(60),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(best.map(|item| item.id.as_str()), Some("healthy"));
    }

    #[test]
    fn best_account_uses_refresh_when_quotas_in_same_tier() {
        // 5h 55% vs 62% 属同一档（50-74），不应让 7 个点的差异盖过重置时间
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "higher-pct-later-reset".into(),
                    email: "later@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "lower-pct-sooner-reset".into(),
                    email: "sooner@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "higher-pct-later-reset".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(62),
                        weekly_remaining_percent: Some(55),
                        weekly_refresh_at: Some("2026-04-20T04:00:00Z".into()),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "lower-pct-sooner-reset".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(55),
                        weekly_remaining_percent: Some(62),
                        weekly_refresh_at: Some("2026-04-18T14:00:00Z".into()),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(
            best.map(|item| item.id.as_str()),
            Some("lower-pct-sooner-reset")
        );
    }

    #[test]
    fn best_account_prefers_earlier_weekly_refresh_on_tie() {
        // 5h% / weekly% / credits / last_synced_at 全打平时，应按重置时间早者优先
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "later".into(),
                    email: "later@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "sooner".into(),
                    email: "sooner@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "later".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(50),
                        weekly_refresh_at: Some("2026-04-20T04:00:00Z".into()),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "sooner".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(50),
                        weekly_refresh_at: Some("2026-04-18T14:00:00Z".into()),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(best.map(|item| item.id.as_str()), Some("sooner"));
    }

    #[test]
    fn best_account_treats_missing_refresh_as_worst_on_tie() {
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "unknown-refresh".into(),
                    email: "unknown@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "known-refresh".into(),
                    email: "known@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "unknown-refresh".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(50),
                        weekly_refresh_at: None,
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "known-refresh".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(50),
                        weekly_refresh_at: Some("2099-01-01T00:00:00Z".into()),
                        credits_balance: Some(0.0),
                        last_synced_at: Some(10),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(best.map(|item| item.id.as_str()), Some("known-refresh"));
    }

    #[test]
    fn best_account_excludes_low_weekly_quota_candidates() {
        let state = State {
            version: 1,
            accounts: vec![
                AccountRecord {
                    id: "low-weekly".into(),
                    email: "low@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
                AccountRecord {
                    id: "healthy".into(),
                    email: "healthy@example.com".into(),
                    account_id: None,
                    updated_at: 1,
                    ..AccountRecord::default()
                },
            ],
            usage_cache: BTreeMap::from([
                (
                    "low-weekly".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(100),
                        weekly_remaining_percent: Some(3),
                        ..UsageSnapshot::default()
                    },
                ),
                (
                    "healthy".into(),
                    UsageSnapshot {
                        five_hour_remaining_percent: Some(80),
                        weekly_remaining_percent: Some(60),
                        ..UsageSnapshot::default()
                    },
                ),
            ]),
        };

        let best = choose_best_account(&state);

        assert_eq!(best.map(|item| item.id.as_str()), Some("healthy"));
    }
}
