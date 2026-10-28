//! A small ledger in three currencies over 2024's first quarter: what the
//! charts are drawn from. All values are invented. Not every test binary
//! uses every item.
#![allow(dead_code)]

use chrono::NaiveDate;
use db::{
    assertions::{AssertionSource, BalanceAssertion},
    market::Market,
};
use ledger::journal::Journal;
use ledger_types::currency::Currency;
use sqlx::SqlitePool;
use tempfile::TempDir;

use super::build::{self, Legs};

pub const MAPPING: &str = r#"
[accounts]
"起鼓" = "Equity:Opening-Balances"

[display]
"Assets"              = "資產"
"Assets:Bank"         = "銀行"
"Assets:Bank:Savings" = "活存"
"Assets:Bank:FX"      = "外幣"
"Assets:Unlisted"     = "未上市持股"
"Liabilities"         = "負債"
"Liabilities:Card"    = "信用卡"
"Income"              = "收入"
"Income:Salary"       = "薪水"
"Expenses"            = "支出"
"Expenses:Food"       = "食食"
"Expenses:Food:Lunch" = "中晝"
"Expenses:Rent"       = "厝稅"
"Income:Investment"   = "投資"
"Expenses:Investment" = "投資"
"Expenses:Investment:Loss" = "投資虧損"
"Equity"              = "權益"

[at_cost]
accounts = ["Assets:Unlisted"]

# Not day-to-day: the charts show their result apart.
[investments]
accounts = ["Income:Investment", "Expenses:Investment"]
"#;

pub const TODAY: &str = "2024-03-31";

fn journal() -> Journal {
    use Currency::*;
    let txns: &[(&str, Legs)] = &[
        ("2024-01-01", &[
            ("Assets:Bank:Savings", "10000", TWD),
            ("Equity:Opening-Balances", "-10000", TWD),
        ]),
        ("2024-01-05", &[("Assets:Bank:Savings", "30000", TWD), ("Income:Salary", "-30000", TWD)]),
        // Held at cost: listed apart, in no total.
        ("2024-01-06", &[
            ("Assets:Unlisted:Gamma", "4000", TWD),
            ("Equity:Opening-Balances", "-4000", TWD),
        ]),
        ("2024-01-10", &[("Expenses:Food:Lunch", "500", TWD), ("Liabilities:Card", "-500", TWD)]),
        ("2024-02-01", &[("Expenses:Rent", "12000", TWD), ("Assets:Bank:Savings", "-12000", TWD)]),
        ("2024-02-03", &[
            ("Assets:Bank:FX", "100", USD),
            ("Equity:Conversions", "-100", USD),
            ("Assets:Bank:Savings", "-3000", TWD),
            ("Equity:Conversions", "3000", TWD),
        ]),
        // An investment's loss, then a gain: neither is day-to-day spending.
        ("2024-02-10", &[
            ("Expenses:Investment:Loss", "2000", TWD),
            ("Assets:Bank:Savings", "-2000", TWD),
        ]),
        ("2024-03-15", &[("Income:Investment", "-500", TWD), ("Assets:Bank:Savings", "500", TWD)]),
        // No JPY rate: from March on, net worth cannot be summed.
        ("2024-03-02", &[
            ("Assets:Bank:FX", "5000", JPY),
            ("Equity:Opening-Balances", "-5000", JPY),
        ]),
        // A foreign expense, converted at its own date.
        ("2024-03-05", &[("Expenses:Food", "10", USD), ("Assets:Bank:FX", "-10", USD)]),
    ];
    build::journal(txns)
}

fn assertions() -> Vec<BalanceAssertion> {
    [("Assets:Bank:Savings", "23500"), ("Liabilities:Card", "-500")]
        .into_iter()
        .map(|(account, closing)| BalanceAssertion {
            source: AssertionSource::Counted,
            account: account.into(),
            currency: Currency::TWD,
            period_start: None,
            opening: None,
            period_end: TODAY.parse().unwrap(),
            closing: closing.parse().unwrap(),
        })
        .collect()
}

pub async fn ledger() -> (TempDir, SqlitePool, Market) {
    let (dir, pool, market) = build::load(MAPPING, &journal(), &assertions()).await;
    build::usd_rate(&market, "2024-01-01", 30.0).await;
    (dir, pool, market)
}

pub fn today() -> NaiveDate { TODAY.parse().unwrap() }
