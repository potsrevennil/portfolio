//! T9's review queue over a synthetic ledger loaded by the real load-journal:
//! the server functions called directly, the rendered pages, and what each
//! change leaves in the database. All values are invented.

mod common;

use common::{
    page::{position, visible, Site},
    queue::{ledger, COFFEE, FEE, LUNCH, TAXI},
};
use db::review::StatementAccounts;
use ledger::valuation::AtCost;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use serde_json::Value;
use sqlx::SqlitePool;
use web::{
    journal::{load_journal, JournalList},
    manual::enter_manual,
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
        provide_context(statement_accounts());
        ScopedFuture::new(f())
    });
    call.await
}

/// What the chart would name: the savings account, and a card whose first
/// statement is still to come. Cash is neither.
fn statement_accounts() -> StatementAccounts {
    ["Assets:Bank:Savings", "Liabilities:Card"].into_iter().collect()
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

/// `(posting id, account, amount, origin)` in posting order.
async fn legs(pool: &SqlitePool, id: i64) -> Vec<(i64, String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT p.id, a.path, p.amount, p.origin FROM postings p JOIN accounts a ON a.id = \
         p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
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
        (server::router(options, pool, AtCost::default(), statement_accounts()), dir)
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

// --- Entering by hand
// ---------------------------------------------------------

#[tokio::test]
async fn a_manual_entry_is_an_ordinary_reviewed_transaction() {
    let (_dir, pool) = ledger().await;
    let enter = |category: &str, counter: &str, amount: &str| {
        let (category, counter, amount) =
            (Some(category.to_string()), Some(counter.to_string()), amount.to_string());
        call(&pool, move || {
            enter_manual(
                "2024-03-05".into(),
                amount,
                "TWD".into(),
                "Assets:Cash".into(),
                category,
                counter,
                Some("早餐".into()),
            )
        })
    };
    let id = enter("Expenses:Food", "", "1,250").await.unwrap();
    let rows = legs(&pool, id).await;
    let booked: Vec<_> =
        rows.iter().map(|(_, a, amount, _)| (a.as_str(), amount.as_str())).collect();
    assert_eq!(booked, [("Assets:Cash", "-1250"), ("Expenses:Food", "1250")]);
    let (source, reviewed, narration): (String, bool, String) =
        sqlx::query_as("SELECT source, reviewed, narration FROM transactions WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((source.as_str(), reviewed, narration.as_str()), ("manual", true, "早餐"));
    let events = events(&pool, id).await;
    assert_eq!((events.len(), events[0].0.as_str()), (1, "entered"));
    assert_eq!(events[0].1["before"], Value::Null);
    // It shows in the journal, not in the queue.
    let journal = call(&pool, || load_journal(JournalQuery::account("Assets:Cash"))).await.unwrap();
    assert_eq!(journal.entries[0].narration.as_deref(), Some("早餐"));
    assert_eq!(call(&pool, review_count).await.unwrap(), 4);

    // To another account the user holds: a transfer.
    let transfer = enter("", "Assets:Split:Alpha:Tab", "200").await.unwrap();
    let rows = legs(&pool, transfer).await;
    assert_eq!((rows[0].2.as_str(), rows[1].1.as_str()), ("-200", "Assets:Split:Alpha:Tab"));

    for (category, counter, amount, refusal) in [
        ("Expenses:Food", "Assets:Bank:Savings", "10", "要選一個，也只能選一個"),
        ("", "", "10", "要選一個，也只能選一個"),
        ("Assets:Bank:Savings", "", "10", "類別要是收入或支出"),
        ("Expenses:Food", "", "0", "金額不可為 0"),
        ("", "Assets:Cash", "10", "對方帳戶不可和帳戶相同"),
    ] {
        let error = enter(category, counter, amount).await.unwrap_err().to_string();
        assert!(error.contains(refusal), "{category}/{counter}/{amount}: {error}");
    }

    // Cash spent on a day a count already closed: the count says what the
    // cash held, so the spend is either in it already or belongs later.
    let counted = call(&pool, || {
        enter_manual(
            "2024-01-20".into(),
            "260".into(),
            "TWD".into(),
            "Assets:Cash".into(),
            Some("Expenses:Food".into()),
            None,
            None,
        )
    })
    .await
    .unwrap_err()
    .to_string();
    assert!(counted.contains("「現金」2024-01-31 的餘額變成 740 TWD"), "{counted}");
    assert!(counted.contains("日期該在 2024-01-31 之後"), "{counted}");
    assert!(!counted.contains("balance check"), "{counted}");
}

/// A record forgotten in 天天記帳 and added here later: the 天天記帳 closing
/// it contradicts was only that app's own sum, so the entry is kept and the
/// closing is marked superseded, not checked again.
#[tokio::test]
async fn a_late_record_supersedes_a_tiantian_closing() {
    let (_dir, pool) = ledger().await;
    sqlx::query(
        "INSERT INTO balance_assertion (account_id, currency, source, period_end, closing) SELECT \
         id, 'TWD', 'tiantian', '2024-02-29', '880' FROM accounts WHERE path = 'Assets:Cash'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let id = call(&pool, || {
        enter_manual(
            "2024-02-20".into(),
            "260".into(),
            "TWD".into(),
            "Assets:Cash".into(),
            Some("Expenses:Food".into()),
            None,
            Some("便當".into()),
        )
    })
    .await
    .unwrap();
    assert_eq!(legs(&pool, id).await.len(), 2);
    let superseded: Option<String> = sqlx::query_scalar(
        "SELECT superseded_at FROM balance_assertion WHERE source = 'tiantian' AND period_end = \
         '2024-02-29'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(superseded.is_some());
    // The check, and every import gate after it, no longer holds it.
    let mut conn = pool.acquire().await.unwrap();
    let report = db::check::check(&mut conn).await.unwrap();
    assert!(report.ok(), "{report}");
    // A count still binds: the cash counted on 2024-01-31 refuses a spend
    // before it (tested with its message in the manual-entry test).
}

/// `(account, tags)` of every leg, in posting order.
async fn posting_tags(pool: &SqlitePool, id: i64) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT a.path, p.tags FROM postings p JOIN accounts a ON a.id = p.account_id WHERE \
         p.transaction_id = ? ORDER BY p.id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Spent from a bank account after its last statement: the bank leg waits
/// as 未對帳 for the next statement, as an imported record does. Cash, which
/// no statement covers, does not.
#[tokio::test]
async fn a_hand_entry_on_a_bank_account_waits_for_its_statement() {
    let (_dir, pool) = ledger().await;
    sqlx::query(
        "INSERT INTO balance_assertion (account_id, currency, source, period_end, closing) SELECT \
         id, 'TWD', 'statement', '2024-02-29', '12300' FROM accounts WHERE path = \
         'Assets:Bank:Savings'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let enter = |account: &str, date: &str| {
        let (account, date) = (account.to_string(), date.to_string());
        call(&pool, move || {
            enter_manual(
                date,
                "90".into(),
                "TWD".into(),
                account,
                Some("Expenses:Food".into()),
                None,
                None,
            )
        })
    };

    let bank = enter("Assets:Bank:Savings", "2024-03-10").await.unwrap();
    assert_eq!(posting_tags(&pool, bank).await, [
        ("Assets:Bank:Savings".to_string(), Some("unverified".to_string())),
        ("Expenses:Food".to_string(), None),
    ]);
    let waiting = JournalQuery { unverified: true, ..Default::default() };
    let waiting = call(&pool, || load_journal(waiting)).await.unwrap();
    assert!(waiting.entries.iter().any(|e| e.id == bank && e.unverified && e.reviewed));

    let cash = enter("Assets:Cash", "2024-03-10").await.unwrap();
    assert!(posting_tags(&pool, cash).await.iter().all(|(_, t)| t.is_none()));
    // Inside the statement, the statement's balance decides instead.
    let covered = enter("Assets:Bank:Savings", "2024-02-20").await.unwrap_err().to_string();
    assert!(covered.contains("日期該在 2024-02-29 之後"), "{covered}");
}

/// 未對帳 follows a leg's account through an edit: moved onto a bank account
/// after its last statement, the leg waits for the statement, so the next
/// import verifies it instead of booking the line twice; moved off, the mark
/// goes, from every leg once none is left for a statement to vouch for.
/// Entered by hand on a card no statement has posted to yet, it waits for the
/// first one.
#[tokio::test]
async fn a_hand_entry_awaiting_a_first_statement_is_marked() {
    let (_dir, pool) = ledger().await;
    let card = call(&pool, || {
        enter_manual(
            "2024-03-10".into(),
            "90".into(),
            "TWD".into(),
            "Liabilities:Card".into(),
            Some("Expenses:Food".into()),
            None,
            None,
        )
    })
    .await
    .unwrap();
    assert_eq!(posting_tags(&pool, card).await, [
        ("Liabilities:Card".to_string(), Some("unverified".to_string())),
        ("Expenses:Food".to_string(), None),
    ]);
}

/// The chart names the statement accounts: each numbered account, and where
/// the app's pooled account and its settlement post. No mapping, none.
#[test]
fn statement_accounts_come_from_the_chart() {
    let dir = tempfile::tempdir().unwrap();
    let mapping = dir.path().join("mapping.toml");
    assert_eq!(server::statement_accounts(&mapping).unwrap(), StatementAccounts::default());
    std::fs::write(
        &mapping,
        r#"
        [institution.accounts]
        "111111111111" = "Assets:Bank:Savings"
        [institution]
        app_account = "銀行"
        primary = "Assets:Bank:Main"
        settlement = "Assets:Bank:Settlement"
        settlement_app_account = "交割"
        clearing = "Assets:Bank:Clearing"
        [fallback]
        income = "Income:Uncategorized"
        expense = "Expenses:Uncategorized"
        "#,
    )
    .unwrap();
    let expected: StatementAccounts =
        ["Assets:Bank:Savings", "Assets:Bank:Main", "Assets:Bank:Settlement"].into_iter().collect();
    assert_eq!(server::statement_accounts(&mapping).unwrap(), expected);
}

/// An entry made by mistake can be deleted: it leaves every list and every
/// balance, and its history keeps what it was.
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

    let entry = site.page("/entry").await;
    // A spend by default: an account to pay from and a category, nothing else.
    assert!(
        entry.contains(r#"name="category""#) && !entry.contains(r#"name="counter""#),
        "{entry}"
    );
    position(&visible(&entry), "支出 收入 轉帳");
    position(&visible(&entry), "付款帳戶");
}
