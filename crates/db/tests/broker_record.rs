//! The broker sub-ledger's storage: a record must come back exactly as it
//! went in.

use db::{
    broker::{self, NewRecord},
    import::ensure_account,
};
use ledger::labels::Labels;
use ledger_types::currency::Currency;
use portfolio::broker::{BrokerRecord, RecordKind};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

const ACCOUNT: &str = "Assets:Broker:Test";

async fn fixture() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("test.db").display());
    let pool = db::init_db(&url).await.unwrap();
    ensure_account(&mut *pool.acquire().await.unwrap(), &Labels::default(), ACCOUNT).await.unwrap();
    (dir, pool)
}

/// Every field holds a different value, so two columns that swapped places in
/// the insert would surface here — the columns are all TEXT, and the gate
/// checks only `amount + commission` and never reads `price`.
#[tokio::test]
async fn a_record_round_trips_field_for_field() {
    let (_dir, pool) = fixture().await;
    let record = BrokerRecord {
        kind: RecordKind::Sell,
        trade_date: Some("2026-03-02".parse().unwrap()),
        settle_date: "2026-03-04".parse().unwrap(),
        executed_at: Some("2026-03-02T13:30:05Z".parse().unwrap()),
        symbol: Some("ZZA".into()),
        quantity: dec!(-7),
        price: dec!(11.5),
        amount: dec!(80.5),
        commission: dec!(-0.25),
        currency: Currency::USD,
        description: "an invented sale".into(),
        key: "2026-03-04:sale".into(),
    };
    let new = NewRecord {
        account: ACCOUNT.into(),
        external_ref: format!("test:{}", record.key),
        import_batch_id: None,
        record: record.clone(),
    };
    broker::insert(&mut *pool.acquire().await.unwrap(), &new).await.unwrap();

    let stored = broker::load(&mut *pool.acquire().await.unwrap()).await.unwrap();
    let [held] = stored[ACCOUNT].as_slice() else { panic!("{stored:?}") };
    // `key` comes back as the stored ref, which is how dedup reads it.
    assert_eq!(*held, BrokerRecord { key: new.external_ref.clone(), ..record });
}

#[tokio::test]
async fn a_trade_date_is_filled_in_but_never_overwritten() {
    let (_dir, pool) = fixture().await;
    let undated = BrokerRecord {
        kind: RecordKind::Buy,
        trade_date: None,
        settle_date: "2026-03-04".parse().unwrap(),
        executed_at: None,
        symbol: Some("ZZA".into()),
        quantity: dec!(1),
        price: dec!(10),
        amount: dec!(-10),
        commission: dec!(0),
        currency: Currency::USD,
        description: "an invented purchase".into(),
        key: "2026-03-04:purchase".into(),
    };
    let external_ref = format!("test:{}", undated.key);
    let new = NewRecord {
        account: ACCOUNT.into(),
        external_ref: external_ref.clone(),
        import_batch_id: None,
        record: undated,
    };
    let mut conn = pool.acquire().await.unwrap();
    broker::insert(&mut conn, &new).await.unwrap();

    let traded = "2026-03-02".parse().unwrap();
    assert!(broker::fill_trade_date(&mut conn, &external_ref, traded).await.unwrap());
    assert!(
        !broker::fill_trade_date(&mut conn, &external_ref, "2026-01-01".parse().unwrap())
            .await
            .unwrap(),
        "a date already held stands"
    );
    let stored = broker::load(&mut conn).await.unwrap();
    assert_eq!(stored[ACCOUNT][0].trade_date, Some(traded));
}
