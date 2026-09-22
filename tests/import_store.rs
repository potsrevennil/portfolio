use portfolio::{
    currency::Currency,
    ledger::{labels::Labels, model::Source},
    store::import::{ImportStore, InsertOutcome, Posting, Transaction},
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

async fn fixture() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let url = format!("sqlite:{}", path.to_str().unwrap());
    let pool = portfolio::init_db(&url).await.unwrap();
    (dir, pool)
}

fn balanced(amount: Decimal) -> Vec<Posting> {
    vec![
        ("Assets:Cash", -amount, Currency::TWD).into(),
        ("Expenses:Food", amount, Currency::TWD).into(),
    ]
}

fn txn(source: Source, external_ref: Option<&str>, postings: Vec<Posting>) -> Transaction {
    Transaction {
        date: "2026-03-01".parse().unwrap(),
        payee: Some("shop".into()),
        narration: None,
        source,
        external_ref: external_ref.map(str::to_string),
        import_batch_id: None,
        postings,
    }
}

async fn count(store: &ImportStore) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM transactions").fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn duplicate_within_a_source_is_rejected_same_ref_across_sources_is_allowed() {
    let (_dir, pool) = fixture().await;
    let store = ImportStore::new(pool, Labels::default());
    let ps = balanced(dec!(100));

    let first = store.insert_deduped(&txn(Source::Import, Some("L1"), ps.clone())).await.unwrap();
    let first_id = first.inserted_id().expect("first insert should write");

    let again = store.insert_deduped(&txn(Source::Import, Some("L1"), ps.clone())).await.unwrap();
    assert_eq!(again, InsertOutcome::Duplicate(first_id));
    // A duplicate wrote nothing but still names the row it matched.
    assert_eq!(again.inserted_id(), None);
    assert_eq!((first.transaction_id(), again.transaction_id()), (first_id, first_id));

    let other = store.insert_deduped(&txn(Source::Manual, Some("L1"), ps)).await.unwrap();
    assert!(matches!(other, InsertOutcome::Inserted(id) if id != first_id));

    assert_eq!(count(&store).await, 2);
}

#[tokio::test]
async fn null_external_ref_is_exempt_and_always_inserts() {
    let (_dir, pool) = fixture().await;
    let store = ImportStore::new(pool, Labels::default());
    let ps = balanced(dec!(50));

    let a = store.insert_deduped(&txn(Source::Manual, None, ps.clone())).await.unwrap();
    let b = store.insert_deduped(&txn(Source::Manual, None, ps)).await.unwrap();
    assert!(matches!(a, InsertOutcome::Inserted(_)));
    assert!(matches!(b, InsertOutcome::Inserted(_)));
    assert_ne!(a.inserted_id(), b.inserted_id());
}

#[tokio::test]
async fn unbalanced_postings_are_rejected_and_write_nothing() {
    let (_dir, pool) = fixture().await;
    let store = ImportStore::new(pool, Labels::default());

    let unbalanced = vec![
        ("Assets:Cash", dec!(-100), Currency::TWD).into(),
        ("Expenses:Food", dec!(99), Currency::TWD).into(),
    ];
    assert!(store.insert_deduped(&txn(Source::Import, Some("BAD"), unbalanced)).await.is_err());
    assert_eq!(count(&store).await, 0);
    let accounts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts").fetch_one(store.pool()).await.unwrap();
    assert_eq!(accounts, 0, "a rejected transaction creates no accounts");
}

#[tokio::test]
async fn a_new_account_and_its_ancestors_get_the_mapping_labels() {
    let (_dir, pool) = fixture().await;
    let labels = Labels::parse(
        r#"
[accounts]
"現金" = "Assets:Cash"
[display]
"Assets" = "資產"
"#,
    )
    .unwrap();
    let store = ImportStore::new(pool, labels);
    store.insert_deduped(&txn(Source::Import, Some("L1"), balanced(dec!(10)))).await.unwrap();

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT path, label FROM accounts ORDER BY path")
            .fetch_all(store.pool())
            .await
            .unwrap();
    let rows: Vec<(&str, &str)> = rows.iter().map(|(p, l)| (p.as_str(), l.as_str())).collect();
    assert_eq!(rows, [
        ("Assets", "資產"),
        ("Assets:Cash", "現金"),
        ("Expenses", "Expenses"),
        ("Expenses:Food", "Food")
    ]);
}
