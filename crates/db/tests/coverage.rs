//! What the coverage page reads and keeps: the figures from both assertion
//! tables, the records on each account, and statement schedules. All values
//! are invented.

mod common;

use db::{
    assertions::{self, AssertionSource, BalanceAssertion},
    broker,
    coverage::{self, Figure, Record, Schedule},
    holdings::{self, HoldingAssertion},
    import::ensure_account,
};
use ledger::{journal::UNVERIFIED_TAG, labels::Labels};
use ledger_types::{cadence::Cadence, currency::Currency};
use portfolio::broker::{BrokerRecord, Commodity, RecordKind};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

const BANK: &str = "Assets:Bank:Test";
const BROKER: &str = "Assets:Broker:Test";

async fn fixture() -> (SqlitePool, tempfile::NamedTempFile) {
    let (pool, file) = common::create_temp_db().await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    for path in [BANK, "Assets:Bank:Test:Sub", BROKER, "Income:Salary"] {
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

#[tokio::test]
async fn figures_come_from_both_tables_until_superseded() {
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

    let figure =
        |account: &str, commodity: &str, broker, source, start: Option<&str>, as_of: &str| Figure {
            account: account.into(),
            commodity: commodity.into(),
            held_to_broker: broker,
            source,
            period_start: start.map(|d| d.parse().unwrap()),
            as_of: as_of.parse().unwrap(),
        };
    use AssertionSource::*;
    let statement = figure(BANK, "TWD", false, Statement, Some("2024-01-01"), "2024-01-31");
    let counted = figure(BANK, "TWD", false, Counted, None, "2024-02-10");
    let held = figure(BROKER, "ZZA", true, Statement, None, "2024-03-31");
    let listed = coverage::figures(&mut conn).await.unwrap();
    assert_eq!(listed, [
        statement.clone(),
        counted.clone(),
        figure(BANK, "TWD", false, Tiantian, None, "2024-02-29"),
        held.clone(),
    ]);

    // A superseded closing no longer vouches for anything.
    assertions::supersede(&mut conn, &tiantian).await.unwrap();
    assert_eq!(coverage::figures(&mut conn).await.unwrap(), [statement, counted, held]);
}

#[tokio::test]
async fn ledger_records_are_per_account_and_carry_the_unverified_mark() {
    let (pool, _file) = fixture().await;
    let conn = &mut *pool.acquire().await.unwrap();
    let checked = common::post(conn, "2024-01-05", BANK, "-10", "TWD", None).await;
    let waiting = common::post(
        conn,
        "2024-01-06",
        "Assets:Bank:Test:Sub",
        "-20",
        "TWD",
        Some(UNVERIFIED_TAG),
    )
    .await;
    let deleted = common::post(conn, "2024-01-07", BANK, "-30", "TWD", None).await;
    sqlx::query("UPDATE transactions SET deleted_at = '2024-01-08' WHERE id = ?")
        .bind(deleted)
        .execute(&mut *conn)
        .await
        .unwrap();
    common::post(conn, "2024-01-09", "Income:Salary", "0", "TWD", None).await;

    // Income is no account a statement checks.
    let records = coverage::ledger_records(conn).await.unwrap();
    let record = |id, account: &str, date: &str, amount, unverified| Record {
        id,
        account: account.into(),
        date: date.parse().unwrap(),
        commodity: "TWD".into(),
        amount,
        unverified,
    };
    assert_eq!(records, [
        record(checked, BANK, "2024-01-05", dec!(-10), false),
        record(waiting, "Assets:Bank:Test:Sub", "2024-01-06", dec!(-20), true)
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

    let records = coverage::broker_records(&mut conn).await.unwrap();
    let rows: Vec<_> = records.iter().map(|r| (r.id, r.commodity.as_str())).collect();
    assert_eq!(rows, [(deposit, "USD"), (buy, "USD"), (buy, "ZZA")]);
}

#[tokio::test]
async fn a_schedule_is_saved_and_replaced() {
    let (pool, _file) = fixture().await;
    let mut conn = pool.acquire().await.unwrap();
    let card = Schedule {
        account: BANK.into(),
        bank: Some("甲銀行".into()),
        cadence: Cadence::Monthly { day: 17 },
        grace: 10,
        since: None,
    };
    coverage::set_schedule(&mut conn, &card).await.unwrap();
    let yearly = Schedule {
        cadence: Cadence::Yearly { month: 12, day: 31 },
        grace: 30,
        since: Some("2022-09-01".parse().unwrap()),
        ..card
    };
    coverage::set_schedule(&mut conn, &yearly).await.unwrap();
    // A blank bank falls back to the chart.
    let unnamed = Schedule {
        account: BROKER.into(),
        bank: Some(" ".into()),
        cadence: Cadence::Never,
        grace: 10,
        since: None,
    };
    coverage::set_schedule(&mut conn, &unnamed).await.unwrap();
    assert_eq!(coverage::schedules(&mut conn).await.unwrap(), [yearly, Schedule {
        bank: None,
        ..unnamed
    }]);

    let stray = Schedule {
        account: "Assets:Nowhere".into(),
        bank: None,
        cadence: Cadence::Quarterly,
        grace: 10,
        since: None,
    };
    assert!(coverage::set_schedule(&mut conn, &stray).await.is_err());
}

#[tokio::test]
async fn a_balance_is_confirmed_only_where_the_ledger_agrees() {
    let (pool, _file) = fixture().await;
    let conn = &mut *pool.acquire().await.unwrap();
    common::post(conn, "2024-01-05", BANK, "120", "TWD", None).await;
    let day = "2024-01-31".parse().unwrap();
    let refused = coverage::confirm(conn, BANK, day, Currency::TWD, dec!(100)).await.unwrap_err();
    assert_eq!(refused.to_string(), "帳簿 2024-01-31 是 120 TWD，毋是 100");
    coverage::confirm(conn, BANK, day, Currency::TWD, dec!(120)).await.unwrap();
    let counted = coverage::figures(conn).await.unwrap();
    assert_eq!(counted.iter().map(|f| (f.source, f.as_of)).collect::<Vec<_>>(), [(
        AssertionSource::Counted,
        day
    )]);
    let closing: String = sqlx::query_scalar("SELECT closing FROM balance_assertion")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(closing, "120");
}
