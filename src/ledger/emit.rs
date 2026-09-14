//! Turning 天天記帳 records into Beancount postings.

use std::collections::BTreeSet;

use super::{accounts, daily, names::fallback_account, writer};

/// A per-record correction, when mapping.toml names this record.
///
/// The one place overrides are looked up. A record reaches the ledger by one of
/// two routes — matched against a bank statement, or taken from the app as
/// written — and a correction that applied to only one of them would fail
/// silently, because both routes produce a valid ledger either way.
///
/// Records the `used` set, so `build` can report an id that matched nothing.
fn corrected(
    chart: &accounts::Chart,
    id: &str,
    used: &mut BTreeSet<String>,
) -> Option<(String, Vec<String>)> {
    let found = chart.override_for(id)?;
    used.insert(id.to_string());
    Some((found.account.to_string(), found.tags.iter().map(|t| writer::tag_name(t)).collect()))
}

/// What to narrate a corrected record as, falling back to the app's own 備註.
///
/// Only for paths where one record becomes one transaction. A statement line
/// can match several records at once, and there the narration describes the
/// line.
pub(super) fn narration_for<'a>(chart: &'a accounts::Chart, id: &str, memo: &'a str) -> &'a str {
    match chart.override_for(id) {
        Some(found) if !found.narration.is_empty() => found.narration.as_str(),
        _ => memo,
    }
}

/// Where a 天天記帳 record's other side belongs, plus any tags it carries.
pub(super) fn resolve(
    chart: &accounts::Chart,
    event: &daily::AppEvent,
    description: &str,
    unmapped: &mut BTreeSet<String>,
    used_overrides: &mut BTreeSet<String>,
) -> (String, Vec<String>) {
    if let Some(found) = corrected(chart, &event.id, used_overrides) {
        return found;
    }
    let found = match &event.contra {
        daily::Contra::Category(name) => chart.category(name, event.delta.is_sign_positive()),
        daily::Contra::Account(name) => chart.account(name),
    };
    match found {
        Some(t) => (t.account.to_string(), t.tags.iter().map(|s| writer::tag_name(s)).collect()),
        None => {
            let raw = match &event.contra {
                daily::Contra::Category(n) | daily::Contra::Account(n) => n,
            };
            unmapped.insert(raw.clone());
            (fallback_account(chart, description, event.delta).to_string(), Vec::new())
        }
    }
}

/// The far-side posting for a matched 天天記帳 record.
///
/// A transfer into a foreign-currency account moved a different number of a
/// different unit. Posting the TWD figure there would invent a TWD balance the
/// account never held — which is why IB showed a million TWD it has never seen.
///
/// Both currencies come off the event, so the near side is whatever the record
/// was actually written in. Taking it from the statement instead assumed the
/// two always agree, which holds only while every account is TWD.
pub(super) fn contra_posting(account: String, event: &daily::AppEvent) -> writer::Posting {
    let near = event.currency.as_str();
    match &event.far {
        Some((amount, far_currency)) if far_currency != near && !amount.is_zero() => {
            let signed = if event.delta.is_sign_negative() { *amount } else { -*amount };
            writer::Posting::new(account, signed, far_currency).worth(event.delta.abs(), near)
        }
        _ => writer::Posting::new(account, -event.delta, near),
    }
}

/// The ledger account a 天天記帳 account name maps to.
fn resolve_account(
    chart: &accounts::Chart,
    name: &str,
    used_accounts: &mut BTreeSet<String>,
    unmapped: &mut BTreeSet<String>,
) -> String {
    let account = match chart.account(name) {
        Some(m) => m.account.to_string(),
        None => {
            unmapped.insert(name.to_string());
            "Assets:Unmapped".to_string()
        }
    };
    used_accounts.insert(account.clone());
    account
}

/// Emits the records for accounts with no statement.
///
/// Records touching the statement-driven account are skipped: those are already
/// emitted from the statement side, and replaying them here would double every
/// one. Because the transfer export names both accounts in a single row, the
/// double entry is already present — no pairing is needed, unlike the statement
/// path.
pub(super) fn emit_daily_accounts(
    entries: &[daily::Entry],
    chart: &accounts::Chart,
    used_accounts: &mut BTreeSet<String>,
    unmapped: &mut BTreeSet<String>,
    used_overrides: &mut BTreeSet<String>,
) -> (String, usize) {
    let mut out = String::new();
    let mut count = 0;

    for entry in entries {
        if entry.accounts().contains(&chart.institution.app_account.as_str()) {
            continue;
        }
        match entry {
            daily::Entry::Flow { date, account, amount, currency, category, memo, id } => {
                if amount.is_zero() {
                    continue;
                }
                let asset = resolve_account(chart, account, used_accounts, unmapped);
                // A named correction wins over the category. The app allows one
                // category per record, so an event it can only file as 投資 or
                // 其他 is described here instead.
                let (contra, tags) = match corrected(chart, id, used_overrides) {
                    Some((account, tags)) => {
                        used_accounts.insert(account.clone());
                        (account, tags)
                    }
                    None => match chart.category(category, amount.is_sign_positive()) {
                        Some(m) => {
                            used_accounts.insert(m.account.to_string());
                            (
                                m.account.to_string(),
                                m.tags.iter().map(|t| writer::tag_name(t)).collect::<Vec<_>>(),
                            )
                        }
                        None => {
                            unmapped.insert(category.clone());
                            let fallback = if amount.is_sign_positive() {
                                &chart.fallback.income
                            } else {
                                &chart.fallback.expense
                            };
                            used_accounts.insert(fallback.to_string());
                            (fallback.to_string(), Vec::new())
                        }
                    },
                };
                let narration = narration_for(chart, id, memo);
                out.push_str(&writer::transaction(*date, category, narration, &tags, &[
                    writer::Posting::new(asset, *amount, currency),
                    writer::Posting::new(contra, -amount, currency),
                ]));
                out.push('\n');
                count += 1;
            }
            daily::Entry::Transfer {
                date,
                from,
                out: sent,
                out_currency,
                to,
                inn,
                in_currency,
                memo,
            } => {
                if sent.is_zero() && inn.is_zero() {
                    continue;
                }
                let source = resolve_account(chart, from, used_accounts, unmapped);
                let target = resolve_account(chart, to, used_accounts, unmapped);
                // Cross-currency legs cannot balance on their own, so the
                // receiving side states what it is worth in the sending
                // currency. Both totals are known, so no rate rounding is
                // involved and the transaction balances exactly.
                let credit = writer::Posting::new(target, *inn, in_currency);
                let credit = if out_currency != in_currency {
                    credit.worth(sent.abs(), out_currency)
                } else {
                    credit
                };
                out.push_str(&writer::transaction(*date, "轉帳", memo, &[], &[
                    writer::Posting::new(source, -sent, out_currency),
                    credit,
                ]));
                out.push('\n');
                count += 1;
            }
        }
    }

    (out, count)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    /// Both routes a record can take into the ledger, against one chart.
    fn chart() -> accounts::Chart {
        toml::from_str(
            r#"
            [expenses]
            "投資" = { account = "Expenses:Fees", tags = ["investment"] }
            [accounts]
            "券商" = "Assets:Broker"
            [overrides]
            "LOSS" = { account = "Expenses:Investment:Loss", narration = "虧損", tags = ["crypto-loss"] }
            "#,
        )
        .expect("test chart parses")
    }

    fn flow(id: &str) -> daily::Entry {
        daily::Entry::Flow {
            date: chrono::NaiveDate::from_ymd_opt(2022, 11, 14).unwrap(),
            account: "券商".into(),
            amount: dec!(-41312.11),
            currency: "USD".into(),
            category: "投資".into(),
            memo: String::new(),
            id: id.into(),
        }
    }

    /// The correction has to reach the emitted postings, not just parse.
    /// Without this, the two halves are each tested and the join between
    /// them is not.
    #[test]
    fn an_override_replaces_the_account_tag_and_narration() {
        let (mut used, mut unmapped, mut overrides) = Default::default();
        let (out, count) = emit_daily_accounts(
            &[flow("LOSS")],
            &chart(),
            &mut used,
            &mut unmapped,
            &mut overrides,
        );

        assert_eq!(count, 1);
        assert!(out.contains("Expenses:Investment:Loss"), "override account missing:\n{out}");
        assert!(!out.contains("Expenses:Fees"), "category account still emitted:\n{out}");
        assert!(out.contains("#crypto-loss"), "override tag missing:\n{out}");
        assert!(out.contains("虧損"), "override narration missing:\n{out}");
        assert!(!out.contains("#investment"), "category tag leaked:\n{out}");
        assert!(used.contains("Expenses:Investment:Loss"), "account never opened");
        assert_eq!(overrides.iter().collect::<Vec<_>>(), ["LOSS"], "use not recorded");
    }

    /// An id naming no override leaves the record on its category, unchanged.
    #[test]
    fn an_unnamed_record_keeps_its_category() {
        let (mut used, mut unmapped, mut overrides) = Default::default();
        let (out, _) = emit_daily_accounts(
            &[flow("OTHER")],
            &chart(),
            &mut used,
            &mut unmapped,
            &mut overrides,
        );

        assert!(out.contains("Expenses:Fees"), "category account missing:\n{out}");
        assert!(out.contains("#investment"), "category tag missing:\n{out}");
        assert!(overrides.is_empty(), "recorded a use that did not happen");
    }

    /// A correction naming a record the export does not contain corrects
    /// nothing. Since the id is a 36-character UUID, a typo is the likely cause
    /// and silence the worst response.
    #[test]
    fn an_override_matching_no_record_is_reported() {
        let chart = chart();
        let (mut used, mut unmapped, mut overrides) = Default::default();
        emit_daily_accounts(&[flow("OTHER")], &chart, &mut used, &mut unmapped, &mut overrides);

        assert_eq!(
            chart.stale_overrides(&overrides).iter().collect::<Vec<_>>(),
            ["LOSS"],
            "an override that matched nothing went unreported"
        );
    }
}
