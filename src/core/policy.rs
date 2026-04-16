#![allow(dead_code)]

use crate::core::state::{
    AccountRecord, CURRENT_ACCOUNT_MIN_FIVE_HOUR_PERCENT, LiveIdentity, State, UsageSnapshot,
};

pub fn choose_best_account<'a>(state: &'a State) -> Option<&'a AccountRecord> {
    let mut candidates: Vec<((i64, i64, f64, i64, i64), &AccountRecord)> = state
        .accounts
        .iter()
        .filter_map(|account| {
            let usage = state.usage_cache.get(&account.id)?;
            if usage.needs_relogin {
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
        .find(|account| identity_matches(account, live))?;
    let usage = state.usage_cache.get(&account.id)?;
    is_current_account_usable(usage).then_some(account)
}

pub fn identity_matches(account: &AccountRecord, live: &LiveIdentity) -> bool {
    if account.email.eq_ignore_ascii_case(&live.email) {
        return true;
    }

    match (&account.account_id, &live.account_id) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub fn is_current_account_usable(usage: &UsageSnapshot) -> bool {
    if usage.needs_relogin {
        return false;
    }

    match usage.five_hour_remaining_percent {
        Some(value) => (value as f64) >= CURRENT_ACCOUNT_MIN_FIVE_HOUR_PERCENT,
        None => false,
    }
}

fn build_score(account: &AccountRecord, usage: &UsageSnapshot) -> (i64, i64, f64, i64, i64) {
    (
        quota_score(usage.five_hour_remaining_percent),
        quota_score(usage.weekly_remaining_percent),
        usage.credits_balance.unwrap_or(-1.0),
        usage.last_synced_at.unwrap_or(0),
        account.updated_at,
    )
}

fn quota_score(value: Option<i64>) -> i64 {
    match value {
        Some(value) => 1_000 + value,
        None => -1,
    }
}

trait TotalCmpTuple {
    fn total_cmp(&self, other: &Self) -> std::cmp::Ordering;
}

impl TotalCmpTuple for (i64, i64, f64, i64, i64) {
    fn total_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .cmp(&other.0)
            .then(self.1.cmp(&other.1))
            .then_with(|| self.2.total_cmp(&other.2))
            .then(self.3.cmp(&other.3))
            .then(self.4.cmp(&other.4))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{choose_best_account, choose_current_account, is_current_account_usable};
    use crate::core::state::{AccountRecord, LiveIdentity, State, UsageSnapshot};

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
            }),
        );

        assert_eq!(current.map(|item| item.id.as_str()), Some("current"));
    }

    #[test]
    fn current_account_below_threshold_is_not_usable() {
        let usage = UsageSnapshot {
            five_hour_remaining_percent: Some(19),
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
}
