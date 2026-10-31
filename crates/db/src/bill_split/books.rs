//! What the engine reads from the chart.

use std::collections::BTreeSet;

use ledger::{
    accounts::{Chart, SplitAccounts},
    labels::Labels,
};

use crate::review::StatementAccounts;

/// Where a write-off goes (冇數).
pub const BAD_DEBT: &str = "Expenses:Bad-Debt";

#[derive(Debug)]
pub struct Books {
    pub split_accounts: SplitAccounts,
    /// 未分類, where a detached line's legs return.
    pub uncategorized_income: String,
    pub uncategorized_expense: String,
    /// Where a line's category is only a default: a 天天記帳 record pairing the
    /// line replaces that leg, so no split may hold it.
    pub provisional: BTreeSet<String>,
    pub statements: StatementAccounts,
    /// Names the accounts a person, a netted split or a write-off creates.
    pub labels: Labels,
}

impl From<&Chart> for Books {
    fn from(chart: &Chart) -> Self {
        Books {
            split_accounts: chart.split_accounts.clone(),
            uncategorized_income: chart.fallback.income.to_string(),
            uncategorized_expense: chart.fallback.expense.to_string(),
            provisional: chart.fallback.accounts().map(|a| a.to_string()).collect(),
            statements: chart.into(),
            labels: chart.into(),
        }
    }
}

impl Books {
    /// A split account a person or group can hold: under a root, not the root.
    pub(super) fn holds(&self, path: &str) -> bool {
        self.split_accounts.covers(path)
            && !self.split_accounts.roots.iter().any(|root| root.as_ref() == path)
    }
}
