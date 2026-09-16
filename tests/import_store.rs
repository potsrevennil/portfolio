use portfolio::store::import::{ImportStore, InsertOutcome, NewPosting, NewTransaction};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

async fn fixture() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let url = format!("sqlite:{}", path.to_str().unwrap());
    let pool = portfolio::init_db(&url).await.unwrap();
    (dir, pool)
}

async fn seed_account(pool: &SqlitePool, path: &str) -> i64 {
    sqlx::query("INSERT INTO accounts (path, label, type) VALUES (?, ?, 'asset')")
        .bind(path)
        .bind(path)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

fn balanced(cash: i64, expense: i64, amount: rust_decimal::Decimal) -> Vec<NewPosting> {
    vec![
        NewPosting { account_id: cash, amount: -amount, currency: "TWD".into(), tags: None },
        NewPosting { account_id: expense, amount, currency: "TWD".into(), tags: None },
    ]
}

fn txn(source: &str, external_ref: Option<&str>) -> NewTransaction {
    NewTransaction {
        date: "2026-03-01".parse().unwrap(),
        payee: Some("shop".into()),
        narration: None,
        source: source.into(),
        external_ref: external_ref.map(str::to_string),
    }
}

async fn count(store: &ImportStore) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM transactions").fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn duplicate_within_a_source_is_rejected_same_ref_across_sources_is_allowed() {
    let (_dir, pool) = fixture().await;
    let cash = seed_account(&pool, "Assets:Cash").await;
    let expense = seed_account(&pool, "Expenses:Food").await;
    let store = ImportStore::new(pool);
    let ps = balanced(cash, expense, dec!(100));

    let first = store.insert_deduped(&txn("import", Some("L1")), &ps).await.unwrap();
    let first_id = first.inserted_id().expect("first insert should write");

    let again = store.insert_deduped(&txn("import", Some("L1")), &ps).await.unwrap();
    assert_eq!(again, InsertOutcome::Duplicate(first_id));

    let other = store.insert_deduped(&txn("manual", Some("L1")), &ps).await.unwrap();
    assert!(matches!(other, InsertOutcome::Inserted(id) if id != first_id));

    assert_eq!(count(&store).await, 2);
}

#[tokio::test]
async fn null_external_ref_is_exempt_and_always_inserts() {
    let (_dir, pool) = fixture().await;
    let cash = seed_account(&pool, "Assets:Cash").await;
    let expense = seed_account(&pool, "Expenses:Food").await;
    let store = ImportStore::new(pool);
    let ps = balanced(cash, expense, dec!(50));

    let a = store.insert_deduped(&txn("manual", None), &ps).await.unwrap();
    let b = store.insert_deduped(&txn("manual", None), &ps).await.unwrap();
    assert!(matches!(a, InsertOutcome::Inserted(_)));
    assert!(matches!(b, InsertOutcome::Inserted(_)));
    assert_ne!(a.inserted_id(), b.inserted_id());
}

#[tokio::test]
async fn unbalanced_postings_are_rejected_and_write_nothing() {
    let (_dir, pool) = fixture().await;
    let cash = seed_account(&pool, "Assets:Cash").await;
    let expense = seed_account(&pool, "Expenses:Food").await;
    let store = ImportStore::new(pool);

    let unbalanced = vec![
        NewPosting { account_id: cash, amount: dec!(-100), currency: "TWD".into(), tags: None },
        NewPosting { account_id: expense, amount: dec!(99), currency: "TWD".into(), tags: None },
    ];
    assert!(store.insert_deduped(&txn("import", Some("BAD")), &unbalanced).await.is_err());
    assert_eq!(count(&store).await, 0);
}
