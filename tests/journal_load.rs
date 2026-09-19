//! `journal-load` over a hand-built journal: openings become balanced
//! transactions against the equity plug. All data is invented.

use std::{collections::BTreeMap, str::FromStr};

use chrono::NaiveDate;
use portfolio::{
    currency::Currency,
    db,
    ledger::{journal, load},
};
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool};
use tempfile::TempDir;

fn dec(s: &str) -> Decimal { Decimal::from_str(s).unwrap() }

fn date(s: &str) -> NaiveDate { s.parse().unwrap() }

fn opening(account: &str, amount: &str, currency: Currency) -> journal::Opening {
    journal::Opening {
        account: account.to_string(),
        currency,
        amount: dec(amount),
        date: date("2020-01-01"),
    }
}

fn leg(group: u64, account: &str, amount: &str) -> journal::Posting {
    journal::Posting {
        group,
        source: "manual".to_string(),
        date: date("2020-02-01"),
        payee: None,
        narration: "test".to_string(),
        external_ref: None,
        account: account.to_string(),
        amount: dec(amount),
        currency: Currency::TWD,
        tags: None,
    }
}

async fn load(journal: &journal::Journal) -> anyhow::Result<(TempDir, SqlitePool)> {
    let dir = TempDir::new()?;
    let args = load::Args {
        journal: dir.path().join("journal.csv"),
        database_url: format!("sqlite:{}", dir.path().join("ledger-app.db").display()),
    };
    journal::write(&args.journal, journal)?;
    load::run(&args).await?;
    let pool = db::init_db(&args.database_url).await?;
    Ok((dir, pool))
}

/// Every posting summed per (account, currency).
async fn balances(pool: &SqlitePool) -> anyhow::Result<BTreeMap<(String, String), Decimal>> {
    let rows = sqlx::query(
        "SELECT a.path, p.currency, p.amount FROM postings p JOIN accounts a ON a.id = \
         p.account_id",
    )
    .fetch_all(pool)
    .await?;
    let mut sums = BTreeMap::new();
    for r in rows {
        *sums.entry((r.get(0), r.get(1))).or_default() += dec(&r.get::<String, _>(2));
    }
    Ok(sums)
}

#[tokio::test]
async fn openings_load_as_balanced_equity_transactions() -> anyhow::Result<()> {
    let journal = journal::Journal {
        openings: vec![
            opening("Assets:Bank", "1000", Currency::TWD),
            opening("Assets:Broker", "50", Currency::USD),
            opening("Assets:Broker", "200", Currency::TWD),
            opening("Liabilities:Card", "-300", Currency::TWD),
        ],
        postings: vec![leg(1, "Assets:Bank", "-100"), leg(1, "Expenses:Food", "100")],
    };
    let (_dir, pool) = load(&journal).await?;

    let equity_type: String = sqlx::query("SELECT type FROM accounts WHERE path = ?")
        .bind(load::OPENING_EQUITY)
        .fetch_one(&pool)
        .await?
        .get(0);
    assert_eq!(equity_type, "equity");

    // One two-leg transaction per opening, each zero-sum in its currency.
    let rows = sqlx::query(
        "SELECT t.id, t.date, t.source, p.currency, p.amount FROM transactions t JOIN postings p \
         ON p.transaction_id = t.id WHERE t.external_ref LIKE 'opening:%'",
    )
    .fetch_all(&pool)
    .await?;
    let mut per_txn: BTreeMap<i64, Vec<(String, Decimal)>> = BTreeMap::new();
    for r in &rows {
        assert_eq!(r.get::<String, _>(1), "2020-01-01");
        assert_eq!(r.get::<String, _>(2), "manual");
        per_txn.entry(r.get(0)).or_default().push((r.get(3), dec(&r.get::<String, _>(4))));
    }
    assert_eq!(per_txn.len(), 4);
    for legs in per_txn.values() {
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].0, legs[1].0, "legs in different currencies");
        assert!((legs[0].1 + legs[1].1).is_zero(), "opening does not balance: {legs:?}");
    }
    let broker_usd_ref: i64 =
        sqlx::query("SELECT COUNT(*) FROM transactions WHERE external_ref = ?")
            .bind(load::opening_ref("Assets:Broker", Currency::USD))
            .fetch_one(&pool)
            .await?
            .get(0);
    assert_eq!(broker_usd_ref, 1);

    // A balance is just the sum of postings, opening included.
    let sums = balances(&pool).await?;
    let get = |account: &str, currency: &str| sums[&(account.to_string(), currency.to_string())];
    assert_eq!(get("Assets:Bank", "TWD"), dec("900"));
    assert_eq!(get("Assets:Broker", "USD"), dec("50"));
    assert_eq!(get("Assets:Broker", "TWD"), dec("200"));
    assert_eq!(get("Liabilities:Card", "TWD"), dec("-300"));
    assert_eq!(get(load::OPENING_EQUITY, "TWD"), dec("-900"));
    assert_eq!(get(load::OPENING_EQUITY, "USD"), dec("-50"));
    Ok(())
}

#[tokio::test]
async fn a_second_opening_for_the_same_account_and_currency_is_rejected() -> anyhow::Result<()> {
    let journal = journal::Journal {
        openings: vec![
            opening("Assets:Bank", "1000", Currency::TWD),
            opening("Assets:Bank", "5", Currency::TWD),
        ],
        postings: vec![],
    };
    let err = load(&journal).await.unwrap_err();
    assert!(err.to_string().contains("second opening for Assets:Bank TWD"), "{err:#}");
    Ok(())
}

#[tokio::test]
async fn no_openings_means_no_equity_plug() -> anyhow::Result<()> {
    let journal = journal::Journal {
        openings: vec![],
        postings: vec![leg(1, "Assets:Bank", "-100"), leg(1, "Expenses:Food", "100")],
    };
    let (_dir, pool) = load(&journal).await?;
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM accounts WHERE path = ?")
        .bind(load::OPENING_EQUITY)
        .fetch_one(&pool)
        .await?
        .get(0);
    assert_eq!(count, 0);
    Ok(())
}
