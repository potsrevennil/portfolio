//! Which ledger account a thing belongs to.
//!
//! The rules are policy; the names they resolve to are the user's. Account
//! numbers, ledger account names and the descriptions a bank prints all came
//! out of here and into `mapping.toml`, so this module decides *how* an account
//! is chosen and the config decides *which*.

use anyhow::{Context, Result};
use rust_decimal::Decimal;

use super::accounts::{Account, Chart};

/// The ledger account a statement's own account number refers to.
pub fn statement_account<'a>(chart: &'a Chart, account_no: &str) -> Result<&'a Account> {
    chart
        .institution
        .accounts
        .get(account_no)
        .with_context(|| format!("unknown account {account_no} — add it to institution.accounts"))
}

/// Fallback for lines the bookkeeping app has no record of.
///
/// A bank prints its own wording for interest and fees, so the descriptions
/// worth recognising differ per institution and are configured rather than
/// compiled in. Anything unrecognised is bucketed by direction, which is all
/// the statement alone can tell us.
pub fn fallback_account<'a>(chart: &'a Chart, description: &str, delta: Decimal) -> &'a Account {
    match chart.fallback.descriptions.get(description) {
        Some(account) => account,
        None if delta.is_sign_positive() => &chart.fallback.income,
        None => &chart.fallback.expense,
    }
}
