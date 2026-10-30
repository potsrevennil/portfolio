//! The coverage page's synthetic ledger as it stands on [`TODAY`]: a bank
//! account, a multi-currency account, a card, cash, a broker, prepaid passes,
//! a split account, an instalment and an emptied wallet, some with a statement
//! schedule. All values are invented.
#![allow(dead_code)]

use db::{
    assertions::{AssertionSource, BalanceAssertion},
    coverage::Schedule,
    holdings::HoldingAssertion,
};
use ledger::{
    accounts::SplitAccounts,
    journal::{Journal, UNVERIFIED_TAG},
};
use ledger_types::{
    cadence::{Cadence, GRACE},
    currency::Currency,
};
use portfolio::broker::{BrokerRecord, Commodity, RecordKind};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use tempfile::TempDir;

use super::{
    build::{self, Legs},
    page::Built,
};

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
"Assets:Prepaid"       = "預付"
"Assets:Prepaid:Gift"  = "禮券"
"Assets:Prepaid:Pass"  = "月票"
"Assets:Split"         = "分帳"
"Assets:Split:Alpha"   = "甲公司"
"Liabilities"          = "負債"
"Assets:Old"           = "舊錢包"
"Liabilities:Card"     = "信用卡"
"Liabilities:Instalments"      = "分期"
"Liabilities:Instalments:Kobo" = "閱讀器"
"#;

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
        // Two passes: one counted, one never.
        ("2023-11-01", false, &[
            ("Assets:Prepaid:Pass", "500", TWD),
            ("Equity:Opening-Balances", "-500", TWD),
        ]),
        ("2023-11-01", false, &[
            ("Assets:Prepaid:Gift", "300", TWD),
            ("Equity:Opening-Balances", "-300", TWD),
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
        // Twelve instalments on the card, two paid so far.
        ("2023-12-01", false, &[
            ("Expenses:Food", "1200", TWD),
            ("Liabilities:Instalments:Kobo", "-1200", TWD),
        ]),
        ("2024-01-10", false, &[
            ("Liabilities:Instalments:Kobo", "100", TWD),
            ("Liabilities:Card", "-100", TWD),
        ]),
        ("2024-01-20", false, &[
            ("Liabilities:Instalments:Kobo", "100", TWD),
            ("Liabilities:Card", "-100", TWD),
        ]),
        // Emptied: hidden unless asked for.
        ("2024-01-03", false, &[
            ("Assets:Old", "70", TWD),
            ("Equity:Opening-Balances", "-70", TWD),
        ]),
        ("2024-01-04", false, &[("Expenses:Food", "70", TWD), ("Assets:Old", "-70", TWD)]),
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
    let txns: Vec<_> = TXNS.iter().map(|(date, _, legs)| (*date, *legs)).collect();
    let mut journal = build::journal(&txns);
    for posting in &mut journal.postings {
        if TXNS[posting.group as usize].1 && posting.account.starts_with("Assets") {
            posting.tags = Some(UNVERIFIED_TAG.to_string());
        }
    }
    journal
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
        (Counted, "Assets:Prepaid:Pass", TWD, None, "2024-02-01", "500"),
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

const BROKER: &str = "Assets:Broker:Alpha";

/// A deposit and a purchase, vouched for by the quarter's statement.
async fn broker(pool: &SqlitePool) {
    let conn = &mut *pool.acquire().await.unwrap();
    let mut deposit = super::broker::line(BROKER, RecordKind::Deposit, "2023-11-05", "alpha:1");
    deposit.record.amount = dec!(500);
    let mut buy = super::broker::line(BROKER, RecordKind::Buy, "2023-12-01", "alpha:2");
    buy.record = BrokerRecord {
        symbol: Some("ABC".into()),
        quantity: dec!(2),
        price: dec!(150),
        amount: dec!(-300),
        ..buy.record
    };
    for line in [deposit, buy] {
        db::broker::insert(conn, &line).await.unwrap();
    }
    for (commodity, quantity) in
        [(Commodity::Cash(Currency::USD), dec!(200)), (Commodity::Security("ABC".into()), dec!(2))]
    {
        let held = HoldingAssertion {
            source: AssertionSource::Statement,
            account: BROKER.into(),
            as_of: "2023-12-31".parse().unwrap(),
            commodity,
            quantity,
        };
        db::holdings::insert(conn, &held).await.unwrap();
    }
}

/// The schedules the reader set: the bank and card on one bank's days, cash
/// counted, the broker quarterly. The others are left unscheduled.
fn schedules() -> Vec<Schedule> {
    let schedule = |account: &str, bank: Option<&str>, cadence| Schedule {
        account: account.into(),
        bank: bank.map(Into::into),
        cadence,
        grace: GRACE,
        since: None,
    };
    vec![
        schedule("Assets:Bank:Savings", Some("甲銀行"), Cadence::Monthly { day: 31 }),
        schedule("Liabilities:Card", Some("甲銀行"), Cadence::Monthly { day: 25 }),
        schedule("Assets:Cash", None, Cadence::Count { every: 30 }),
        schedule("Assets:Broker:Alpha", None, Cadence::Quarterly),
        schedule("Liabilities:Instalments:Kobo", None, Cadence::Instalment),
    ]
}

/// The chart's split-account roots, as mapping.toml would give them.
pub fn splits() -> SplitAccounts { SplitAccounts { roots: vec!["Assets:Split".parse().unwrap()] } }

pub struct Ledger {
    pub dir: TempDir,
    pub pool: SqlitePool,
}

impl Built for Ledger {
    fn parts(self) -> (TempDir, SqlitePool) { (self.dir, self.pool) }
}

pub async fn ledger() -> Ledger {
    let (dir, pool, _market) = build::load(MAPPING, &journal(), &assertions()).await;
    broker(&pool).await;
    let conn = &mut *pool.acquire().await.unwrap();
    for s in schedules() {
        db::coverage::set_schedule(conn, &s).await.unwrap();
    }
    Ledger { dir, pool }
}
