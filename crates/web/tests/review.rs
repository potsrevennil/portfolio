//! T9's review queue over a synthetic ledger loaded by the real load-journal:
//! the server functions called directly, the rendered pages, and what each
//! change leaves in the database. All values are invented.

mod common;

use common::{
    page::{position, visible, Site},
    queue::{ledger, COFFEE, FEE, LUNCH, TAXI},
};
use ledger::valuation::AtCost;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use serde_json::Value;
use sqlx::SqlitePool;
use web::{
    journal::JournalList,
    model::{JournalQuery, Queue, Source as TxnSource},
    review::{confirm_entry, load_queue, review_count, Actions},
    server,
};

/// Runs a server function as the server would, with the pool in context.
async fn call<T, F>(pool: &SqlitePool, f: impl FnOnce() -> F) -> Result<T, ServerFnError>
where
    F: std::future::Future<Output = Result<T, ServerFnError>>,
{
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        ScopedFuture::new(f())
    });
    call.await
}

async fn queue(pool: &SqlitePool, query: JournalQuery) -> Queue {
    call(pool, || load_queue(query)).await.unwrap()
}

fn narrations(q: &Queue) -> Vec<String> {
    q.journal.entries.iter().map(|e| e.narration.clone().unwrap_or_default()).collect()
}

async fn id_of(pool: &SqlitePool, narration: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM transactions WHERE narration = ?")
        .bind(narration)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `(kind, payload)` of every event on a transaction, oldest first.
async fn events(pool: &SqlitePool, id: i64) -> Vec<(String, Value)> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind, payload FROM transaction_events WHERE transaction_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(k, p)| (k, serde_json::from_str(&p).unwrap())).collect()
}

async fn reviewed(pool: &SqlitePool, id: i64) -> bool {
    sqlx::query_scalar("SELECT reviewed != 0 FROM transactions WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn with_actions<T>(f: impl FnOnce() -> T) -> T {
    Owner::new().with(|| {
        provide_context(Actions::new());
        f()
    })
}

async fn site() -> Site {
    Site::new(|| async {
        let (dir, pool) = ledger().await;
        let options = LeptosOptions::builder().output_name("web").build();
        (server::router(options, pool, AtCost::default()), dir)
    })
    .await
}

// --- The queue --------------------------------------------------------------

#[tokio::test]
async fn the_queue_lists_what_awaits_review_newest_first() {
    let (_dir, pool) = ledger().await;
    let q = queue(&pool, JournalQuery::default()).await;
    assert_eq!(narrations(&q), [COFFEE, TAXI, LUNCH, FEE]);
    assert_eq!(call(&pool, review_count).await.unwrap(), 4);
    // Whatever review state the URL asks for, the queue holds unreviewed.
    let asked = JournalQuery { review: Some("reviewed".into()), ..Default::default() };
    assert_eq!(queue(&pool, asked).await.journal.total, 4);
}

#[tokio::test]
async fn the_queue_filters_by_account_source_and_date() {
    let (_dir, pool) = ledger().await;
    let bank = queue(&pool, JournalQuery::account("Assets:Bank")).await;
    assert_eq!(narrations(&bank), [COFFEE, FEE]);
    let source = |s: TxnSource| JournalQuery { source: Some(s.to_string()), ..Default::default() };
    assert_eq!(narrations(&queue(&pool, source(TxnSource::Import)).await), [FEE]);
    assert_eq!(narrations(&queue(&pool, source(TxnSource::Tiantian)).await), [COFFEE, TAXI, LUNCH]);
    assert_eq!(queue(&pool, source(TxnSource::Manual)).await.journal.total, 0);
    let dates = JournalQuery {
        from: Some("2024-02-02".into()),
        to: Some("2024-02-03".into()),
        ..Default::default()
    };
    assert_eq!(narrations(&queue(&pool, dates).await), [TAXI, LUNCH]);

    let bad = JournalQuery { source: Some("bank".into()), ..Default::default() };
    let error = call(&pool, || load_queue(bad)).await.unwrap_err().to_string();
    assert!(error.contains("不明的來源：bank"), "{error}");
}

/// Unverified (no statement has checked it) and unreviewed (nobody has
/// looked at it) are two states, each with its own mark.
#[tokio::test]
async fn unverified_is_not_the_same_as_unreviewed() {
    let (_dir, pool) = ledger().await;
    let q = queue(&pool, JournalQuery::default()).await;
    let html = with_actions(|| {
        let journal = q.journal.clone();
        view! { <JournalList action="/review" journal review=true /> }.to_html()
    });
    let rows: Vec<String> = html
        .split(r#"<li class="entry"#)
        .skip(1)
        .map(|e| visible(&e[e.find('>').unwrap() + 1..]))
        .collect();
    assert!(rows[0].starts_with("2024-03-01 咖啡 天天記帳 未對帳 未確認"), "{}", rows[0]);
    assert!(rows[1].starts_with("2024-02-03 計程車 天天記帳 未確認"), "{}", rows[1]);
    assert!(!rows[1].contains("未對帳"), "{}", rows[1]);
    assert_eq!(html.matches(r#"class="entry unverified"#).count(), 1, "{html}");
    // The importer's uncategorised leg says so.
    assert!(
        rows[3].contains("2024-02-01 手續費 對帳單 未確認 活存 -500 TWD 未分類支出 未分類 500 TWD"),
        "{}",
        rows[3]
    );
    // Every row can be confirmed as it is.
    assert_eq!(html.matches(r#"<button type="submit">確認</button>"#).count(), 4, "{html}");
}

// --- Confirming -------------------------------------------------------------

#[tokio::test]
async fn confirming_leaves_the_queue_and_an_event() {
    let (_dir, pool) = ledger().await;
    let lunch = id_of(&pool, LUNCH).await;
    call(&pool, || confirm_entry(lunch)).await.unwrap();
    assert!(reviewed(&pool, lunch).await);
    assert_eq!(narrations(&queue(&pool, JournalQuery::default()).await), [COFFEE, TAXI, FEE]);
    let written = events(&pool, lunch).await;
    assert_eq!(written.len(), 1);
    let (kind, payload) = &written[0];
    assert_eq!(kind, "confirmed");
    assert_eq!(
        (&payload["before"]["reviewed"], &payload["after"]["reviewed"]),
        (&Value::Bool(false), &Value::Bool(true))
    );
    // Confirming it again changes nothing, so records nothing.
    call(&pool, || confirm_entry(lunch)).await.unwrap();
    assert_eq!(events(&pool, lunch).await.len(), 1);
}

// --- Through the router ---------------------------------------------------

#[tokio::test]
async fn the_review_pages_render() {
    let site = site().await;
    let queue = site.page("/review?source=tiantian").await;
    assert!(queue.contains("待確認 · 帳簿"), "{queue}");
    let text = visible(&queue);
    // The count beside the link counts the whole queue; it streams in on
    // its own, so it lands wherever it resolves.
    assert!(queue.contains(r#"<span class="count-badge">4</span>"#), "{queue}");
    position(&text, "共 3 筆");
    let source = &queue[position(&queue, r#"<select name="source""#)..];
    assert!(source.contains(r#"<option value="tiantian" selected"#), "{source}");
    assert!(!queue.contains(r#"name="review""#), "{queue}");
    let journal = site.page("/journal").await;
    assert!(journal.contains(r#"type="checkbox" name="review" value="unreviewed""#), "{journal}");
    position(&visible(&journal), "只看未確認 只看未對帳");
    // Both states explained where they are filtered, one per line.
    for page in [&queue, &journal] {
        let legend = &page[position(page, r#"<ul class="legend">"#)..];
        let legend = &legend[..position(legend, "</ul>")];
        let lines: Vec<_> = legend.split("</li>").map(visible).filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "{legend}");
        assert!(lines[0].starts_with("未確認 你還沒看過"), "{legend}");
        assert!(lines[1].starts_with("未對帳 記了但銀行對帳單還沒出現"), "{legend}");
    }
}
