use std::env;
use std::io::{self, IsTerminal};

use chrono::{DateTime, Local, Utc};
use unicode_width::UnicodeWidthStr;

use super::CodexAdapter;
use crate::core::state::{AccountRecord, AccountType, LiveIdentity, State, UsageSnapshot};
use crate::core::ui as core_ui;

impl CodexAdapter {
    pub fn render_account_table(&self, state: &State, active: Option<&LiveIdentity>) -> String {
        let ui = core_ui::messages();
        if state.accounts.is_empty() {
            return ui.no_usable_account_hint().to_string();
        }

        let mut accounts = state.accounts.iter().collect::<Vec<_>>();
        accounts.sort_by(|left, right| {
            account_type_sort_key(left)
                .cmp(&account_type_sort_key(right))
                .then_with(|| left.email.cmp(&right.email))
        });
        let mut usable_count = 0usize;
        let has_api_account = accounts.iter().any(|account| account.is_api());

        let rows = accounts
            .into_iter()
            .map(|account| {
                let usage = state
                    .usage_cache
                    .get(&account.id)
                    .cloned()
                    .unwrap_or_default();
                if account.is_subscription() && account_is_usable(&usage) {
                    usable_count += 1;
                }
                let leading_cells = vec![
                    if active.is_some_and(|live| active_matches(account, live)) {
                        active_account_marker()
                    } else {
                        String::new()
                    },
                    account.email.clone(),
                    format_account_type(account),
                ];

                if account.is_api() {
                    TableRow::WithSpan {
                        leading_cells,
                        span_start: 3,
                        span_columns: 5,
                        span_value: "N/A".into(),
                        span_align: "center",
                    }
                } else {
                    let plan = account
                        .plan
                        .clone()
                        .or(usage.plan.clone())
                        .unwrap_or_else(|| ui.unknown().into());
                    TableRow::Cells(
                        leading_cells
                            .into_iter()
                            .chain([
                                plan,
                                format_quota_percent(usage.five_hour_remaining_percent),
                                format_quota_percent(usage.weekly_remaining_percent),
                                format_reset_on(usage.weekly_refresh_at.as_deref()),
                                format_account_status(&usage),
                            ])
                            .collect(),
                    )
                }
            })
            .collect::<Vec<_>>();

        if usable_count == 0 && !has_api_account {
            ui.no_usable_account_hint().to_string()
        } else {
            render_table(
                &ui.table_headers(),
                &rows,
                &[
                    "center", "left", "center", "center", "center", "center", "center", "center",
                ],
                Some(ui.usable_account_summary(usable_count)),
            )
        }
    }
}

fn account_type_sort_key(account: &AccountRecord) -> u8 {
    match account.account_type {
        AccountType::Subscription => 0,
        AccountType::Api => 1,
    }
}

fn active_matches(account: &AccountRecord, live: &LiveIdentity) -> bool {
    if live.scodex_account_id.as_deref() == Some(account.id.as_str()) {
        return true;
    }

    account.email.eq_ignore_ascii_case(&live.email)
        || account.account_id.is_some() && account.account_id == live.account_id
}

fn format_account_type(account: &AccountRecord) -> String {
    let ui = core_ui::messages();
    match account.account_type {
        AccountType::Subscription => {
            if ui.is_zh() {
                "官方订阅".into()
            } else {
                "SUBSCRIPTION".into()
            }
        }
        AccountType::Api => "API".into(),
    }
}

fn format_percent(value: Option<i64>) -> String {
    let ui = core_ui::messages();
    value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| ui.na().into())
}

fn format_quota_percent(value: Option<i64>) -> String {
    let text = format_percent(value);
    match value {
        Some(value) if value < 20 => style_text(&text, AnsiStyle::Red),
        Some(value) if value < 50 => style_text(&text, AnsiStyle::Yellow),
        Some(_) => style_text(&text, AnsiStyle::Green),
        None => text,
    }
}

fn format_reset_on(value: Option<&str>) -> String {
    let ui = core_ui::messages();
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return ui.na().into();
    };
    if value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("null")
        || value.eq_ignore_ascii_case("n/a")
    {
        return ui.na().into();
    }
    if let Ok(timestamp) = value.parse::<i64>() {
        if let Some(parsed) = DateTime::<Utc>::from_timestamp(timestamp, 0) {
            return parsed
                .with_timezone(&Local)
                .format("%m-%d %H:%M")
                .to_string();
        }
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return parsed
            .with_timezone(&Local)
            .format("%m-%d %H:%M")
            .to_string();
    }
    ui.na().into()
}

fn format_account_status(usage: &UsageSnapshot) -> String {
    let ui = core_ui::messages();
    if usage.needs_relogin {
        style_text(ui.status_relogin(), AnsiStyle::Red)
    } else if usage.last_sync_error.is_some() {
        style_text(ui.status_error(), AnsiStyle::Red)
    } else {
        style_text(ui.status_ok(), AnsiStyle::Green)
    }
}

fn account_is_usable(usage: &UsageSnapshot) -> bool {
    !usage.needs_relogin && usage.last_sync_error.is_none()
}

fn active_account_marker() -> String {
    "✓".into()
}

fn render_table(
    headers: &[&str],
    rows: &[TableRow],
    aligns: &[&str],
    summary: Option<String>,
) -> String {
    let widths = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| row.cell_width(index))
                .fold(visible_width(header), usize::max)
        })
        .collect::<Vec<_>>();

    let render_border = |left: char, middle: char, right: char| {
        format!(
            "{}{}{}",
            left,
            widths
                .iter()
                .map(|width| "─".repeat(width + 2))
                .collect::<Vec<_>>()
                .join(&middle.to_string()),
            right
        )
    };
    let render_row_border = |left: char, right: char, upper: &TableRow, lower: &TableRow| {
        render_transition_border(&widths, left, right, Some(upper), Some(lower))
    };
    let render_summary_border = |left: char, right: char, upper: &TableRow| {
        render_summary_transition_border(&widths, left, right, upper)
    };

    let render_cells = |values: Vec<String>| {
        let cells = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| align_cell(value, widths[index], aligns[index]))
            .collect::<Vec<_>>();
        format!("│ {} │", cells.join(" │ "))
    };
    let render_row = |row: &TableRow| match row {
        TableRow::Cells(values) => render_cells(values.clone()),
        TableRow::WithSpan {
            leading_cells,
            span_start,
            span_columns,
            span_value,
            span_align,
        } => {
            let mut rendered = leading_cells
                .iter()
                .enumerate()
                .map(|(index, value)| align_cell(value.clone(), widths[index], aligns[index]))
                .collect::<Vec<_>>();
            let span_width = widths[*span_start..(*span_start + *span_columns)]
                .iter()
                .sum::<usize>()
                + (*span_columns - 1) * 3;
            rendered.push(align_cell(span_value.clone(), span_width, span_align));
            format!("│ {} │", rendered.join(" │ "))
        }
    };

    let mut lines = vec![
        render_border('┌', '┬', '┐'),
        render_cells(headers.iter().map(|item| (*item).to_string()).collect()),
        render_border('├', '┼', '┤'),
    ];
    for (index, row) in rows.iter().enumerate() {
        lines.push(render_row(row));
        if index + 1 != rows.len() {
            lines.push(render_row_border('├', '┤', row, &rows[index + 1]));
        }
    }
    if let Some(summary) = summary {
        let total_width = widths.iter().sum::<usize>() + (widths.len() - 1) * 3;
        let total_inner = total_width + 2;
        if let Some(last_row) = rows.last() {
            lines.push(render_summary_border('├', '┤', last_row));
        } else {
            lines.push(format!("├{}┤", "─".repeat(total_inner)));
        }
        let summary = align_cell(summary, total_width, "center");
        lines.push(format!("│ {} │", summary));
        lines.push(format!("└{}┘", "─".repeat(total_inner)));
    } else {
        lines.push(render_border('└', '┴', '┘'));
    }
    lines.join("\n")
}

#[derive(Debug, Clone)]
enum TableRow {
    Cells(Vec<String>),
    WithSpan {
        leading_cells: Vec<String>,
        span_start: usize,
        span_columns: usize,
        span_value: String,
        span_align: &'static str,
    },
}

impl TableRow {
    fn cell_width(&self, index: usize) -> usize {
        match self {
            TableRow::Cells(values) => values.get(index).map_or(0, |value| visible_width(value)),
            TableRow::WithSpan {
                leading_cells,
                span_start,
                span_value,
                ..
            } => {
                if index < *span_start {
                    return leading_cells
                        .get(index)
                        .map_or(0, |value| visible_width(value));
                }
                if index == *span_start {
                    return visible_width(span_value);
                }
                0
            }
        }
    }

    fn has_boundary_after(&self, index: usize) -> bool {
        match self {
            TableRow::Cells(_) => true,
            TableRow::WithSpan {
                span_start,
                span_columns,
                ..
            } => {
                let span_end = span_start + span_columns - 1;
                !(index >= *span_start && index < span_end)
            }
        }
    }
}

fn render_transition_border(
    widths: &[usize],
    left: char,
    right: char,
    upper: Option<&TableRow>,
    lower: Option<&TableRow>,
) -> String {
    let mut line = String::new();
    line.push(left);
    for (index, width) in widths.iter().enumerate() {
        line.push_str(&"─".repeat(width + 2));
        if index + 1 == widths.len() {
            continue;
        }
        let upper_boundary = upper.is_some_and(|row| row.has_boundary_after(index));
        let lower_boundary = lower.is_some_and(|row| row.has_boundary_after(index));
        line.push(match (upper_boundary, lower_boundary) {
            (true, true) => '┼',
            (true, false) => '┴',
            (false, true) => '┬',
            (false, false) => '─',
        });
    }
    line.push(right);
    line
}

fn render_summary_transition_border(
    widths: &[usize],
    left: char,
    right: char,
    upper: &TableRow,
) -> String {
    let mut line = String::new();
    line.push(left);
    for (index, width) in widths.iter().enumerate() {
        line.push_str(&"─".repeat(width + 2));
        if index + 1 == widths.len() {
            continue;
        }
        line.push(if upper.has_boundary_after(index) {
            '┴'
        } else {
            '─'
        });
    }
    line.push(right);
    line
}

fn align_cell(value: String, width: usize, align: &str) -> String {
    let value_width = visible_width(&value);
    let padding = width.saturating_sub(value_width);
    match align {
        "left" => format!("{value}{}", " ".repeat(padding)),
        "right" => format!("{}{}", " ".repeat(padding), value),
        "center" => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{}{}", " ".repeat(left), value, " ".repeat(right))
        }
        _ => value,
    }
}

fn visible_width(value: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi_codes(value).as_str())
}

fn strip_ansi_codes(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        result.push(ch);
    }
    result
}

fn style_enabled() -> bool {
    io::stdout().is_terminal()
        && env::var_os("NO_COLOR").is_none()
        && !matches!(env::var("TERM").ok().as_deref(), Some("dumb"))
}

#[derive(Debug, Clone, Copy)]
enum AnsiStyle {
    Red,
    Yellow,
    Green,
}

fn style_text(value: &str, style: AnsiStyle) -> String {
    if !style_enabled() {
        return value.to_string();
    }
    let code = match style {
        AnsiStyle::Red => "31",
        AnsiStyle::Yellow => "33",
        AnsiStyle::Green => "32",
    };
    format!("\u{1b}[{code}m{value}\u{1b}[0m")
}

#[cfg(test)]
mod tests {
    use super::{TableRow, render_table, strip_ansi_codes, visible_width};
    use crate::adapters::codex::CodexAdapter;
    use crate::core::state::{AccountRecord, AccountType, State, UsageSnapshot};

    #[test]
    fn strip_ansi_codes_keeps_visible_width_correct() {
        let styled = "\u{1b}[32m80%\u{1b}[0m";
        assert_eq!(strip_ansi_codes(styled), "80%");
        assert_eq!(visible_width(styled), 3);
    }

    #[test]
    fn table_uses_unicode_borders() {
        let rendered = render_table(
            &["A", "B"],
            &[TableRow::Cells(vec!["1".into(), "2".into()])],
            &["left", "left"],
            Some("1 usable account(s)".into()),
        );
        assert!(rendered.contains('┌'));
        assert!(rendered.contains('┬'));
        assert!(rendered.contains('└'));
        assert!(rendered.contains('│'));
    }

    #[test]
    fn table_can_render_summary_without_rows() {
        let rendered = render_table(&["A", "B"], &[], &["left", "left"], Some("0 usable".into()));
        assert!(rendered.contains("0 usable"));
        assert!(rendered.contains('┌'));
        assert!(rendered.contains('└'));
    }

    #[test]
    fn render_account_table_returns_empty_state_message_without_accounts() {
        let adapter = CodexAdapter;
        let rendered = adapter.render_account_table(&State::default(), None);
        assert_eq!(
            rendered,
            crate::core::ui::messages().no_usable_account_hint()
        );
    }

    #[test]
    fn render_account_table_returns_empty_state_message_when_no_account_is_usable() {
        let adapter = CodexAdapter;
        let mut state = State::default();
        state.accounts.push(AccountRecord {
            id: "acct-1".into(),
            email: "a@example.com".into(),
            auth_path: "/tmp/auth.json".into(),
            ..Default::default()
        });
        state.usage_cache.insert(
            "acct-1".into(),
            UsageSnapshot {
                last_sync_error: Some("quota api failed".into()),
                ..Default::default()
            },
        );

        let rendered = adapter.render_account_table(&state, None);
        assert_eq!(
            rendered,
            crate::core::ui::messages().no_usable_account_hint()
        );
    }

    #[test]
    fn render_account_table_shows_api_accounts_with_na_quota() {
        let adapter = CodexAdapter;
        let mut state = State::default();
        state.accounts.push(AccountRecord {
            id: "api".into(),
            account_type: AccountType::Api,
            email: "56wxyz@openrouter".into(),
            auth_path: "/tmp/auth.json".into(),
            ..Default::default()
        });

        let rendered = adapter.render_account_table(&state, None);

        assert!(rendered.contains("Type"));
        assert!(rendered.contains("API"));
        assert!(rendered.contains("56wxyz@openrouter"));
        let api_line = rendered
            .lines()
            .find(|line| line.contains("56wxyz@openrouter"))
            .expect("api row");
        assert_eq!(api_line.matches("N/A").count(), 1);
        assert_eq!(api_line.matches('│').count(), 5);
    }

    #[test]
    fn render_account_table_merges_api_row_borders() {
        let adapter = CodexAdapter;
        let mut state = State::default();
        state.accounts.push(AccountRecord {
            id: "subscription".into(),
            email: "a@example.com".into(),
            plan: Some("Plus".into()),
            auth_path: "/tmp/subscription-auth.json".into(),
            ..Default::default()
        });
        state.accounts.push(AccountRecord {
            id: "api".into(),
            account_type: AccountType::Api,
            email: "56wxyz@openrouter".into(),
            auth_path: "/tmp/api-auth.json".into(),
            ..Default::default()
        });
        state.usage_cache.insert(
            "subscription".into(),
            UsageSnapshot {
                five_hour_remaining_percent: Some(100),
                weekly_remaining_percent: Some(100),
                ..Default::default()
            },
        );

        let rendered = adapter.render_account_table(&state, None);
        let lines = rendered.lines().collect::<Vec<_>>();
        let api_index = lines
            .iter()
            .position(|line| line.contains("56wxyz@openrouter"))
            .expect("api row");
        let border_above = lines[api_index - 1];
        let border_below = lines[api_index + 1];

        assert_eq!(border_above.matches('┼').count(), 3);
        assert_eq!(border_above.matches('┴').count(), 4);
        assert_eq!(border_below.matches('┴').count(), 3);
        assert_eq!(border_above.matches('┤').count(), 1);
        assert!(border_above.ends_with('┤'));
        assert!(border_below.starts_with('├'));
        assert!(border_below.ends_with('┤'));
    }

    #[test]
    fn render_account_table_orders_subscription_accounts_before_api_accounts() {
        let adapter = CodexAdapter;
        let mut state = State::default();
        state.accounts.push(AccountRecord {
            id: "api".into(),
            account_type: AccountType::Api,
            email: "a-api@example.com".into(),
            auth_path: "/tmp/api-auth.json".into(),
            ..Default::default()
        });
        state.accounts.push(AccountRecord {
            id: "subscription".into(),
            email: "z-subscription@example.com".into(),
            plan: Some("Plus".into()),
            auth_path: "/tmp/subscription-auth.json".into(),
            ..Default::default()
        });
        state.usage_cache.insert(
            "subscription".into(),
            UsageSnapshot {
                five_hour_remaining_percent: Some(100),
                weekly_remaining_percent: Some(100),
                ..Default::default()
            },
        );

        let rendered = adapter.render_account_table(&state, None);
        let subscription_index = rendered
            .find("z-subscription@example.com")
            .expect("subscription row");
        let api_index = rendered.find("a-api@example.com").expect("api row");

        assert!(subscription_index < api_index);
    }

    #[test]
    fn render_account_table_places_summary_inside_table_footer() {
        let adapter = CodexAdapter;
        let mut state = State::default();
        state.accounts.push(AccountRecord {
            id: "acct-1".into(),
            email: "a@example.com".into(),
            plan: Some("Plus".into()),
            auth_path: "/tmp/auth.json".into(),
            ..Default::default()
        });
        state.usage_cache.insert(
            "acct-1".into(),
            UsageSnapshot {
                five_hour_remaining_percent: Some(100),
                weekly_remaining_percent: Some(47),
                weekly_refresh_at: Some("2026-04-20T15:32:00Z".into()),
                ..Default::default()
            },
        );

        let rendered = adapter.render_account_table(&state, None);
        let lines = rendered.lines().collect::<Vec<_>>();
        let summary = crate::core::ui::messages().usable_account_summary(1);

        // transition border before summary
        assert_eq!(lines[lines.len() - 3].chars().next(), Some('├'));
        // summary row
        assert_eq!(lines[lines.len() - 2].chars().next(), Some('│'));
        assert!(lines[lines.len() - 2].contains(&summary));
        // clean bottom border (no column dividers)
        assert_eq!(lines.last().and_then(|line| line.chars().next()), Some('└'));
        assert!(!lines.last().unwrap().contains('┴'));
    }
}
