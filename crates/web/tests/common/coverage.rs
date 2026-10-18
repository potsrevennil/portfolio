//! The coverage page's synthetic ledger as it stands on [`TODAY`]: a bank
//! account, a multi-currency account, a card, cash and a broker, each with
//! its own cadence. All values are invented.
#![allow(dead_code)]

use db::assertions::{AssertionSource, BalanceAssertion};
use ledger::{
    coverage::Sources,
    journal::{Journal, Posting, UNVERIFIED_TAG},
};
use ledger_types::currency::Currency;
use sqlx::SqlitePool;
use tempfile::TempDir;
use web::progress::Config;

use super::fixture::{self, posting};

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
                tags: (*unverified && account.starts_with("Assets"))
                    .then(|| UNVERIFIED_TAG.to_string()),
                ..posting(group, date, account, amount, *currency)
            })
        })
        .collect();
    Journal { postings }
}

/// (source, account, currency, (period start, opening), period end, closing)
type Figure = (
    AssertionSource,
    &'static str,
    Currency,
    Option<(&'static str, &'static str)>,
    &'static str,
    &'static str,
);

fn assertions() -> Vec<BalanceAssertion> {
    use AssertionSource::*;
    use Currency::*;
    let figures: &[Figure] = &[
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
    let (dir, pool) = fixture::load(MAPPING, &journal(), &assertions()).await;
    sqlx::raw_sql(BROKER).execute(&pool).await.unwrap();
    let manifest = dir.path().join("manifest.csv");
    std::fs::write(&manifest, MANIFEST.join("\n")).unwrap();
    let config =
        Config { sources: Sources::load(dir.path().join("mapping.toml")).unwrap(), manifest };
    Ledger { dir, pool, config }
}
