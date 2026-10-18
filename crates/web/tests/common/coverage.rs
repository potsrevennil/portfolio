//! The coverage page's synthetic ledger: one source in each state it can be
//! in on [`TODAY`], loaded by the real load-journal. All values are invented.
#![allow(dead_code)]

use std::path::PathBuf;

use db::{
    assertions::{AssertionSource, BalanceAssertion},
    load,
};
use ledger::{
    coverage::Sources,
    journal::{self, Journal, Posting, UNVERIFIED_TAG},
    model::Source,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;
use web::progress::Config;

pub const TODAY: &str = "2024-03-10";

pub const MAPPING: &str = r#"
[display]
"Assets"               = "資產"
"Assets:Bank"          = "銀行"
"Assets:Bank:Savings"  = "活存"
"Assets:Bank:FX"       = "外幣"
"Assets:Broker"        = "券商"
"Assets:Broker:Alpha"  = "甲券商"
"Assets:Cash"          = "現金"
"Assets:Split"         = "分帳"
"Assets:Split:Alpha"   = "甲公司"
"Liabilities"          = "負債"
"Liabilities:Card"     = "信用卡"

[[sources]]
account = "Assets:Bank:Savings"
cadence = "on-demand"
raw     = "bank/savings"

[[sources]]
account = "Assets:Bank:FX"
cadence = "on-demand"

[[sources]]
account = "Liabilities:Card"
label   = "甲卡"
cadence = "monthly"
day     = 25
raw     = "card"
url     = "https://card.example/statements"

[[sources]]
account = "Assets:Cash"
cadence = "count"
every   = 30

[[sources]]
account = "Assets:Broker:Alpha"
cadence = "quarterly"
"#;

const MANIFEST: &[&str] = &[
    "raw_path,original,sha256,bytes,downloaded",
    "raw/card/2023-12.csv,a.csv,00,1,",
    "raw/card/2024-01.csv,b.csv,00,1,2024-02-03",
    "raw/cards/2024-02.csv,c.csv,00,1,2024-03-03",
];

type Legs = &'static [(&'static str, &'static str, Currency)];

/// (date, unverified, legs)
const TXNS: &[(&str, bool, Legs)] = {
    use Currency::*;
    &[
        ("2023-11-01", false, &[
            ("Assets:Bank:Savings", "10000", TWD),
            ("Equity:Opening-Balances", "-10000", TWD),
        ]),
        ("2023-11-01", false, &[
            ("Assets:Cash", "1000", TWD),
            ("Equity:Opening-Balances", "-1000", TWD),
        ]),
        // Money sent to the broker: its own records live apart.
        ("2023-11-01", false, &[
            ("Assets:Broker:Alpha", "1000", TWD),
            ("Equity:Opening-Balances", "-1000", TWD),
        ]),
        ("2023-12-01", false, &[
            ("Assets:Bank:FX", "100", USD),
            ("Equity:Opening-Balances", "-100", USD),
        ]),
        ("2023-12-01", false, &[
            ("Assets:Bank:FX", "5000", JPY),
            ("Equity:Opening-Balances", "-5000", JPY),
        ]),
        ("2023-12-20", false, &[("Expenses:Food", "300", TWD), ("Liabilities:Card", "-300", TWD)]),
        ("2024-01-05", false, &[
            ("Assets:Split:Alpha", "200", TWD),
            ("Assets:Bank:Savings", "-200", TWD),
        ]),
        ("2024-01-05", false, &[
            ("Assets:Bank:FX", "50", EUR),
            ("Equity:Opening-Balances", "-50", EUR),
        ]),
        ("2024-01-10", false, &[
            ("Expenses:Food", "500", TWD),
            ("Assets:Bank:Savings", "-500", TWD),
        ]),
        // Spent to nothing: the EUR statement's zero leaves nothing to check.
        ("2024-01-15", false, &[("Expenses:Food", "50", EUR), ("Assets:Bank:FX", "-50", EUR)]),
        ("2024-01-20", false, &[("Expenses:Food", "200", TWD), ("Liabilities:Card", "-200", TWD)]),
        ("2024-02-02", false, &[("Expenses:Food", "100", TWD), ("Assets:Cash", "-100", TWD)]),
        ("2024-02-15", false, &[
            ("Assets:Bank:Savings", "1000", TWD),
            ("Income:Salary", "-1000", TWD),
        ]),
        ("2024-03-05", true, &[
            ("Expenses:Food", "150", TWD),
            ("Assets:Bank:Savings", "-150", TWD),
        ]),
    ]
};

fn journal() -> Journal {
    let postings = TXNS
        .iter()
        .enumerate()
        .flat_map(|(group, (date, unverified, legs))| {
            legs.iter().map(move |(account, amount, currency)| Posting {
                group: group as u64,
                source: Source::Manual,
                date: date.parse().unwrap(),
                payee: None,
                narration: "test".into(),
                external_ref: None,
                account: account.to_string(),
                amount: amount.parse::<Decimal>().unwrap(),
                currency: *currency,
                tags: (*unverified && account.starts_with("Assets"))
                    .then(|| UNVERIFIED_TAG.to_string()),
            })
        })
        .collect();
    Journal { postings }
}

fn assertions() -> Vec<BalanceAssertion> {
    use AssertionSource::*;
    use Currency::*;
    // (source, account, currency, (period start, opening), period end, closing)
    let figures: &[(AssertionSource, &str, Currency, Option<(&str, &str)>, &str, &str)] = &[
        // Two statements nine days apart.
        (Statement, "Assets:Bank:Savings", TWD, Some(("2023-11-01", "0")), "2024-01-31", "9300"),
        (
            Statement,
            "Assets:Bank:Savings",
            TWD,
            Some(("2024-02-10", "9300")),
            "2024-02-29",
            "10300",
        ),
        (Statement, "Assets:Bank:FX", USD, Some(("2023-12-01", "0")), "2024-02-29", "100"),
        (Statement, "Assets:Bank:FX", JPY, Some(("2023-12-01", "0")), "2023-12-31", "5000"),
        (Statement, "Assets:Bank:FX", EUR, Some(("2024-01-01", "0")), "2024-01-15", "0"),
        (Statement, "Liabilities:Card", TWD, Some(("2023-11-26", "0")), "2023-12-25", "-300"),
        (Counted, "Assets:Cash", TWD, None, "2024-01-31", "1000"),
        // That app's own sum, which vouches for nothing here.
        (Tiantian, "Assets:Cash", TWD, None, "2024-02-29", "900"),
    ];
    figures
        .iter()
        .map(|(source, account, currency, period, end, closing)| BalanceAssertion {
            source: *source,
            account: account.to_string(),
            currency: *currency,
            period_start: period.map(|(start, _)| start.parse().unwrap()),
            opening: period.map(|(_, opening)| opening.parse().unwrap()),
            period_end: end.parse().unwrap(),
            closing: closing.parse().unwrap(),
        })
        .collect()
}

/// A deposit and a purchase, vouched for by the quarter's statement.
const BROKER: &str = "
INSERT INTO broker_record (account_id, external_ref, kind, settle_date, symbol, quantity, price,
                           amount, commission, currency, description)
SELECT id, 'alpha:1', 'deposit', '2023-11-05', NULL, '0', '0', '500', '0', 'USD', 'deposit'
FROM accounts WHERE path = 'Assets:Broker:Alpha'
UNION ALL
SELECT id, 'alpha:2', 'buy', '2023-12-01', 'ABC', '2', '150', '-300', '0', 'USD', 'buy'
FROM accounts WHERE path = 'Assets:Broker:Alpha';
INSERT INTO holding_assertion (account_id, as_of, kind, commodity, quantity, source)
SELECT id, '2023-12-31', 'cash', 'USD', '200', 'statement' FROM accounts
WHERE path = 'Assets:Broker:Alpha'
UNION ALL
SELECT id, '2023-12-31', 'position', 'ABC', '2', 'statement' FROM accounts
WHERE path = 'Assets:Broker:Alpha';
";

pub struct Ledger {
    pub dir: TempDir,
    pub pool: SqlitePool,
    pub config: Config,
}

pub async fn ledger() -> Ledger {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let mapping = root.join("mapping.toml");
    std::fs::write(&mapping, MAPPING).unwrap();
    let manifest: PathBuf = root.join("manifest.csv");
    std::fs::write(&manifest, MANIFEST.join("\n")).unwrap();
    let journal_path = root.join("journal.csv");
    journal::write(&journal_path, &journal()).unwrap();
    journal::write_assertions(&journal::assertions_path(&journal_path), &assertions()).unwrap();
    let url = format!("sqlite:{}", root.join("ledger-app.db").display());
    load::run(&load::Args {
        journal: journal_path,
        database_url: url.clone(),
        mapping: mapping.clone(),
    })
    .await
    .unwrap();
    let pool = db::init_db(&url).await.unwrap();
    sqlx::raw_sql(BROKER).execute(&pool).await.unwrap();
    let config = Config { sources: Sources::load(&mapping).unwrap(), manifest };
    Ledger { dir, pool, config }
}
