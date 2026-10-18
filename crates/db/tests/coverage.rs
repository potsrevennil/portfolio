//! What the coverage page reads: outside figures from both assertion tables,
//! and the records under a source. All values are invented.

mod common;

use db::{
    assertions::{self, AssertionSource, BalanceAssertion},
    broker,
    coverage::{self, Figure, Record},
    holdings::{self, HoldingAssertion},
    import::ensure_account,
};
use ledger::{journal::UNVERIFIED_TAG, labels::Labels};
use ledger_types::currency::Currency;
use portfolio::broker::{BrokerRecord, Commodity, RecordKind};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

const BANK: &str = "Assets:Bank:Test";
const BROKER: &str = "Assets:Broker:Test";

async fn fixture() -> (SqlitePool, tempfile::NamedTempFile) {
    let (pool, file) = common::create_temp_db().await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    for path in [BANK, "Assets:Bank:Test:Sub", BROKER, "Expenses:Food"] {
        ensure_account(&mut conn, &Labels::default(), path).await.unwrap();
    }
    drop(conn);
    (pool, file)
}

fn balance(source: AssertionSource, end: &str, closing: &str) -> BalanceAssertion {
    BalanceAssertion {
        source,
        account: BANK.into(),
        currency: Currency::TWD,
        period_start: None,
        opening: None,
        period_end: end.parse().unwrap(),
        closing: closing.parse().unwrap(),
    }
}

/// One transaction: `amount` on `account` against food, tagged as given.
async fn spend(
    pool: &SqlitePool,
    date: &str,
    account: &str,
    amount: &str,
    tags: Option<&str>,
) -> i64 {
    let id = sqlx::query("INSERT INTO transactions (date, source) VALUES (?, 'manual')")
        .bind(date)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    for (path, amount, tags) in
        [(account, format!("-{amount}"), tags), ("Expenses:Food", amount.into(), None)]
    {
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency, tags)
             SELECT ?, id, ?, 'TWD', ? FROM accounts WHERE path = ?",
        )
        .bind(id)
        .bind(amount)
        .bind(tags)
        .bind(path)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

#[tokio::test]
async fn figures_come_from_outside_parties_only() {
    let (pool, _file) = fixture().await;
    let mut conn = pool.acquire().await.unwrap();
    let statement = BalanceAssertion {
        period_start: Some("2024-01-01".parse().unwrap()),
        opening: Some(dec!(0)),
        ..balance(AssertionSource::Statement, "2024-01-31", "100")
    };
    let tiantian = balance(AssertionSource::Tiantian, "2024-02-29", "90");
    for a in [&statement, &balance(AssertionSource::Counted, "2024-02-10", "0"), &tiantian] {
        assertions::insert(&mut conn, a).await.unwrap();
    }
    holdings::insert(&mut conn, &HoldingAssertion {
        source: AssertionSource::Statement,
        account: BROKER.into(),
        as_of: "2024-03-31".parse().unwrap(),
        commodity: Commodity::Security("ZZA".into()),
        quantity: dec!(2),
    })
    .await
    .unwrap();

    let figures = coverage::figures(&mut conn).await.unwrap();
    let figure =
        |account: &str, kind: &str, source: &str, start: Option<&str>, as_of: &str, figure| {
            Figure {
                account: account.into(),
                commodity: if kind == "position" { "ZZA" } else { "TWD" }.into(),
                kind: kind.into(),
                source: source.into(),
                period_start: start.map(|d| d.parse().unwrap()),
                as_of: as_of.parse().unwrap(),
                figure,
            }
        };
    // 天天記帳's own closing is no outside figure.
    assert_eq!(figures, [
        figure(BANK, "balance", "statement", Some("2024-01-01"), "2024-01-31", dec!(100)),
        figure(BANK, "balance", "counted", None, "2024-02-10", dec!(0)),
        figure(BROKER, "position", "statement", None, "2024-03-31", dec!(2)),
    ]);

    // The view no longer offers a superseded closing to any reader.
    assertions::supersede(&mut conn, &tiantian).await.unwrap();
    let listed = holdings::coverage(&mut conn).await.unwrap();
    assert!(listed.iter().all(|c| c.source != "tiantian"), "{listed:?}");
}

#[tokio::test]
async fn ledger_records_cover_the_subtree_and_carry_the_unverified_mark() {
    let (pool, _file) = fixture().await;
    let checked = spend(&pool, "2024-01-05", BANK, "10", None).await;
    let waiting =
        spend(&pool, "2024-01-06", "Assets:Bank:Test:Sub", "20", Some(UNVERIFIED_TAG)).await;
    let deleted = spend(&pool, "2024-01-07", BANK, "30", None).await;
    sqlx::query("UPDATE transactions SET deleted_at = '2024-01-08' WHERE id = ?")
        .bind(deleted)
        .execute(&pool)
        .await
        .unwrap();
    spend(&pool, "2024-01-09", "Expenses:Food", "0", None).await;

    let records = coverage::ledger_records(&mut pool.acquire().await.unwrap(), BANK).await.unwrap();
    let record = |id, account: &str, date: &str, unverified| Record {
        id,
        account: account.into(),
        date: date.parse().unwrap(),
        commodity: "TWD".into(),
        unverified,
    };
    assert_eq!(records, [
        record(checked, BANK, "2024-01-05", false),
        record(waiting, "Assets:Bank:Test:Sub", "2024-01-06", true)
    ]);
}

#[tokio::test]
async fn a_broker_record_moves_cash_and_the_security() {
    let (pool, _file) = fixture().await;
    let mut conn = pool.acquire().await.unwrap();
    let line = |kind, symbol: Option<&str>, quantity, amount, key| {
        let mut line = common::broker::line(BROKER, kind, "2024-02-02", key);
        line.record =
            BrokerRecord { symbol: symbol.map(Into::into), quantity, amount, ..line.record };
        line
    };
    let deposit =
        broker::insert(&mut conn, &line(RecordKind::Deposit, None, dec!(0), dec!(50), "a"))
            .await
            .unwrap();
    let buy =
        broker::insert(&mut conn, &line(RecordKind::Buy, Some("ZZA"), dec!(1), dec!(-20), "b"))
            .await
            .unwrap();

    let records = coverage::broker_records(&mut conn, BROKER).await.unwrap();
    let rows: Vec<_> = records.iter().map(|r| (r.id, r.commodity.as_str())).collect();
    assert_eq!(rows, [(deposit, "USD"), (buy, "USD"), (buy, "ZZA")]);
}
