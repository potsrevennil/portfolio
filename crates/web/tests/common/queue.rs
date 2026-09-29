//! The review queue's synthetic ledger, loaded by the real load-journal.
//! Reviewed history, then four records awaiting review. All values are
//! invented. Not every test binary uses every item.
#![allow(dead_code)]

use db::{
    assertions::{AssertionSource, BalanceAssertion},
    load,
};
use ledger::{
    journal::{self, Journal, Posting, UNVERIFIED_TAG},
    model::Source,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;

pub const MAPPING: &str = r#"
[display]
"Assets"                 = "資產"
"Assets:Bank"            = "銀行"
"Assets:Bank:Savings"    = "活存"
"Assets:Cash"            = "現金"
"Assets:Old-Wallet"      = "舊錢包"
"Assets:Split"           = "分帳"
"Assets:Split:Alpha"     = "甲公司"
"Assets:Split:Alpha:Tab" = "分帳"
"Liabilities"            = "負債"
"Liabilities:Card"       = "信用卡"
"Income"                 = "收入"
"Income:Salary"          = "薪水"
"Expenses"               = "支出"
"Expenses:Food"          = "吃飯"
"Expenses:Uncategorized" = "未分類支出"
"Equity"                 = "權益"
"#;

pub const FEE: &str = "手續費";
pub const LUNCH: &str = "午餐";
pub const TAXI: &str = "計程車";
pub const COFFEE: &str = "咖啡";

type Legs = &'static [(&'static str, &'static str)];

struct Txn {
    date: &'static str,
    narration: &'static str,
    source: Source,
    unverified: bool,
    legs: Legs,
}

const fn txn(date: &'static str, narration: &'static str, source: Source, legs: Legs) -> Txn {
    Txn { date, narration, source, unverified: false, legs }
}

/// Reviewed history, then four records awaiting review: a bank fee the
/// importer left uncategorised, a 天天記帳 lunch in cash, a taxi on the card,
/// and a coffee from the bank no statement has checked yet.
const TXNS: &[Txn] = &[
    txn("2024-01-01", "期初", Source::Manual, &[
        ("Assets:Bank:Savings", "10000"),
        ("Equity:Opening-Balances", "-10000"),
    ]),
    txn("2024-01-01", "期初", Source::Manual, &[
        ("Assets:Cash", "1000"),
        ("Equity:Opening-Balances", "-1000"),
    ]),
    txn("2024-01-02", "薪水", Source::Import, &[
        ("Assets:Bank:Savings", "3000"),
        ("Income:Salary", "-3000"),
    ]),
    // Emptied, so it can be closed.
    txn("2024-01-03", "舊錢包", Source::Tiantian, &[
        ("Assets:Old-Wallet", "70"),
        ("Assets:Cash", "-70"),
    ]),
    txn("2024-01-04", "舊錢包", Source::Tiantian, &[
        ("Assets:Old-Wallet", "-70"),
        ("Assets:Cash", "70"),
    ]),
    txn("2024-02-01", FEE, Source::Import, &[
        ("Assets:Bank:Savings", "-500"),
        ("Expenses:Uncategorized", "500"),
    ]),
    txn("2024-02-02", LUNCH, Source::Tiantian, &[
        ("Expenses:Food", "120"),
        ("Assets:Cash", "-120"),
    ]),
    txn("2024-02-03", TAXI, Source::Tiantian, &[
        ("Expenses:Uncategorized", "300"),
        ("Liabilities:Card", "-300"),
    ]),
    Txn {
        unverified: true,
        ..txn("2024-03-01", COFFEE, Source::Tiantian, &[
            ("Expenses:Food", "150"),
            ("Assets:Bank:Savings", "-150"),
        ])
    },
    txn("2024-01-05", "代墊", Source::Import, &[
        ("Assets:Split:Alpha:Tab", "200"),
        ("Assets:Bank:Savings", "-200"),
    ]),
];

pub const UNREVIEWED: &[&str] = &[FEE, LUNCH, TAXI, COFFEE];

pub fn journal() -> Journal {
    let postings = TXNS
        .iter()
        .enumerate()
        .flat_map(|(group, t)| {
            t.legs.iter().map(move |(account, amount)| Posting {
                group: group as u64,
                source: t.source,
                date: t.date.parse().unwrap(),
                payee: None,
                narration: t.narration.into(),
                external_ref: None,
                account: account.to_string(),
                amount: amount.parse::<Decimal>().unwrap(),
                currency: Currency::TWD,
                tags: t.unverified.then(|| UNVERIFIED_TAG.to_string()),
            })
        })
        .collect();
    Journal { postings }
}

/// The bank's statement closes February, the cash count January: an edit
/// that moves either is one the gate refuses.
pub fn assertions() -> Vec<BalanceAssertion> {
    [("Assets:Bank:Savings", "2024-02-29", "12300"), ("Assets:Cash", "2024-01-31", "1000")]
        .into_iter()
        .map(|(account, day, closing)| BalanceAssertion {
            source: AssertionSource::Counted,
            account: account.into(),
            currency: Currency::TWD,
            period_start: None,
            opening: None,
            period_end: day.parse().unwrap(),
            closing: closing.parse().unwrap(),
        })
        .collect()
}

pub async fn ledger() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("mapping.toml"), MAPPING).unwrap();
    let journal_path = root.join("journal.csv");
    journal::write(&journal_path, &journal()).unwrap();
    journal::write_assertions(&journal::assertions_path(&journal_path), &assertions()).unwrap();
    let url = format!("sqlite:{}", root.join("ledger-app.db").display());
    load::run(&load::Args {
        journal: journal_path,
        database_url: url.clone(),
        mapping: root.join("mapping.toml"),
    })
    .await
    .unwrap();
    let pool = db::init_db(&url).await.unwrap();
    for narration in UNREVIEWED {
        sqlx::query("UPDATE transactions SET reviewed = 0 WHERE narration = ?")
            .bind(narration)
            .execute(&pool)
            .await
            .unwrap();
    }
    // As the importer books an uncategorised line.
    sqlx::query(
        "UPDATE postings SET origin = 'fallback' WHERE account_id = (SELECT id FROM accounts \
         WHERE path = 'Expenses:Uncategorized')",
    )
    .execute(&pool)
    .await
    .unwrap();
    (dir, pool)
}
