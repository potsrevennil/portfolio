//! The chart of accounts, every account including the ones with no postings
//! (the tree's group levels).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use sqlx::SqlitePool;

use super::query::AccountType;

#[derive(Debug, Clone, PartialEq)]
pub struct ChartAccount {
    pub path: String,
    pub label: String,
    pub account_type: AccountType,
    pub closed: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    path: String,
    label: String,
    acct_type: String,
    closed: i64,
}

/// Every account, ordered by path.
pub async fn accounts(pool: &SqlitePool) -> Result<Vec<ChartAccount>> {
    let rows: Vec<Row> =
        sqlx::query_as("SELECT path, label, type AS acct_type, closed FROM accounts ORDER BY path")
            .fetch_all(pool)
            .await
            .context("loading the chart of accounts")?;
    rows.into_iter()
        .map(|r| {
            Ok(ChartAccount {
                account_type: r
                    .acct_type
                    .parse()
                    .with_context(|| format!("account type {:?} is invalid", r.acct_type))?,
                path: r.path,
                label: r.label,
                closed: r.closed != 0,
            })
        })
        .collect()
}

/// Labels for use outside a tree, where no parent row gives context: a label
/// several accounts share takes its parent's in front (甲公司分帳 vs
/// 乙公司分帳), unless the parent is a root. Fava's `uniqueName`
/// (`ledger/MandarinOverview.js`). Keyed by path.
pub fn standalone_labels(chart: &[ChartAccount]) -> BTreeMap<String, String> {
    let labels: BTreeMap<&str, &str> =
        chart.iter().map(|a| (a.path.as_str(), a.label.as_str())).collect();
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for a in chart.iter().filter(|a| a.path.rsplit(':').next() != Some(a.label.as_str())) {
        *count.entry(a.label.as_str()).or_default() += 1;
    }
    chart
        .iter()
        .map(|a| {
            let shared = count.get(a.label.as_str()).is_some_and(|n| *n > 1);
            let label = match a.path.rsplit_once(':') {
                Some((parent, _)) if shared && parent.contains(':') => {
                    format!("{}{}", labels.get(parent).copied().unwrap_or(parent), a.label)
                }
                _ => a.label.clone(),
            };
            (a.path.clone(), label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(path: &str, label: &str) -> ChartAccount {
        ChartAccount {
            path: path.into(),
            label: label.into(),
            account_type: AccountType::Asset,
            closed: false,
        }
    }

    #[test]
    fn a_shared_label_takes_its_parents_in_front() {
        let chart = [
            account("Assets", "資產"),
            account("Assets:Split", "分帳"),
            account("Assets:Split:Alpha", "甲公司"),
            account("Assets:Split:Alpha:Tab", "分帳"),
            account("Assets:Split:Beta", "乙公司"),
            account("Assets:Split:Beta:Tab", "分帳"),
            account("Assets:Cash", "現金"),
            // Leaves shown as-is are never "shared".
            account("Assets:Tab", "Tab"),
            account("Assets:Cash:Tab", "Tab"),
        ];
        let labels = standalone_labels(&chart);
        assert_eq!(labels["Assets:Split:Alpha:Tab"], "甲公司分帳");
        assert_eq!(labels["Assets:Split:Beta:Tab"], "乙公司分帳");
        // Under a root there is no parent to add.
        assert_eq!(labels["Assets:Split"], "分帳");
        assert_eq!(labels["Assets:Cash"], "現金");
        assert_eq!(labels["Assets:Cash:Tab"], "Tab");
    }
}
