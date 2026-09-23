//! What an importer reads and writes around a record it verifies.

use db::{
    import::{ImportStore, Posting, Transaction},
    import_batch::{self, Verification},
};
use ledger::{journal::UNVERIFIED_TAG, labels::Labels, model::Source};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

const LINE: &str = "Assets:Bank:LineBank";
const OTHER: &str = "Assets:Bank:Main:Savings";

/// A transfer between two accounts that each have statements, recorded by the
/// app and checked by neither yet: both legs unverified.
async fn transfer(pool: SqlitePool) -> (SqlitePool, i64) {
    let store = ImportStore::new(pool, Labels::default());
    let tagged = |account: &str, amount| Posting {
        tags: Some(UNVERIFIED_TAG.to_string()),
        ..(account, amount, Currency::TWD).into()
    };
    let id = store
        .insert_deduped(&Transaction {
            date: "2026-09-29".parse().expect("date"),
            payee: None,
            narration: None,
            source: Source::Tiantian,
            external_ref: Some("tiantian:1".into()),
            import_batch_id: None,
            postings: vec![tagged(LINE, dec!(-200)), tagged(OTHER, dec!(200))],
        })
        .await
        .expect("insert")
        .inserted_id()
        .expect("inserted");
    (store.pool().clone(), id)
}

async fn tagged_accounts(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT a.path FROM postings p JOIN accounts a ON a.id = p.account_id
         WHERE p.tags LIKE '%' || ? || '%' ORDER BY a.path",
    )
    .bind(UNVERIFIED_TAG)
    .fetch_all(pool)
    .await
    .expect("tagged legs")
}

/// One bank's line vouches for its own leg only: the other account's leg, which
/// no statement has shown yet, keeps the tag.
#[tokio::test]
async fn verifying_leaves_an_unchecked_leg_tagged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite:{}", dir.path().join("verify.db").display());
    let (pool, id) = transfer(db::init_db(&url).await.expect("db")).await;
    let mut db = pool.acquire().await.expect("connection");

    let held = import_batch::postings(&mut db, LINE, Currency::TWD).await.expect("postings");
    let leg = match held.as_slice() {
        [leg] => leg,
        other => panic!("one leg on {LINE}, not {}", other.len()),
    };
    assert!(leg.unverified);
    assert_eq!(leg.refs, ["tiantian:1"]);
    assert_eq!(leg.other_accounts, [OTHER]);

    import_batch::verify(&mut db, &Verification {
        transaction_id: id,
        date: Some("2026-10-01".parse().expect("date")),
        statement_ref: "line-bank:1".into(),
        unchecked: vec![OTHER.to_string()],
    })
    .await
    .expect("verify");
    drop(db);
    assert_eq!(tagged_accounts(&pool).await, [OTHER]);

    let (date, external_ref): (String, String) =
        sqlx::query_as("SELECT date, external_ref FROM transactions WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("the record");
    // The record keeps the ref its own source dedups on, and stands for the
    // line as well.
    assert_eq!((date.as_str(), external_ref.as_str()), ("2026-10-01", "tiantian:1"));
    let mut db = pool.acquire().await.expect("connection");
    let refs = import_batch::refs(&mut db, "line-bank:").await.expect("refs");
    assert!(refs.contains("line-bank:1"), "{refs:?}");
}

/// With both banks' lines in one import, nothing is left unchecked.
#[tokio::test]
async fn verifying_every_leg_clears_the_tag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite:{}", dir.path().join("both.db").display());
    let (pool, id) = transfer(db::init_db(&url).await.expect("db")).await;
    let mut db = pool.acquire().await.expect("connection");
    for external_ref in ["line-bank:1", "cathay-bank:1"] {
        import_batch::verify(&mut db, &Verification {
            transaction_id: id,
            date: Some("2026-10-01".parse().expect("date")),
            statement_ref: external_ref.into(),
            unchecked: Vec::new(),
        })
        .await
        .expect("verify");
    }
    drop(db);
    assert!(tagged_accounts(&pool).await.is_empty());
}

/// Verifying with no date leaves the record where the bank that booked it
/// first put it, and still clears its leg's tag.
#[tokio::test]
async fn verifying_without_a_date_moves_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite:{}", dir.path().join("dateless.db").display());
    let (pool, id) = transfer(db::init_db(&url).await.expect("db")).await;
    let mut db = pool.acquire().await.expect("connection");
    import_batch::verify(&mut db, &Verification {
        transaction_id: id,
        date: None,
        statement_ref: "line-bank:1".into(),
        unchecked: vec![OTHER.to_string()],
    })
    .await
    .expect("verify");
    drop(db);

    let date: String = sqlx::query_scalar("SELECT date FROM transactions WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("the record");
    assert_eq!(date, "2026-09-29");
    assert_eq!(tagged_accounts(&pool).await, [OTHER]);
}
