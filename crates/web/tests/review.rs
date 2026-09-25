//! T9's review queue over a synthetic ledger loaded by the real load-journal:
//! the server functions called directly, the rendered pages, and what each
//! change leaves in the database. All values are invented.

mod common;

use chrono::NaiveDate;
use common::{
    page::{position, visible, Site},
    queue::{ledger, COFFEE, FEE, LUNCH, TAXI},
};
use db::pairing::{self, Ambiguity};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use rust_decimal_macros::dec;
use serde_json::Value;
use sqlx::SqlitePool;
use web::{
    accounts::{close_account, load_accounts, reopen_account},
    journal::{load_journal, JournalList},
    manual::enter_manual,
    model::{Editing, JournalQuery, LegInput, Origin, Queue, Source as TxnSource},
    review::{
        choose_pairing, confirm_entry, load_entry, load_queue, review_count, save_entry, Actions,
        Editor, SPARE_ROWS,
    },
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

fn leg(posting: Option<i64>, account: &str, amount: &str) -> LegInput {
    LegInput {
        posting: posting.map(|p| p.to_string()).unwrap_or_default(),
        account: account.into(),
        amount: amount.into(),
        currency: "TWD".into(),
    }
}

async fn save(
    pool: &SqlitePool,
    id: i64,
    narration: &str,
    legs: Vec<LegInput>,
    confirm: bool,
) -> Result<(), ServerFnError> {
    let narration = Some(narration.to_string());
    let confirm = confirm.then(|| "1".to_string());
    call(pool, || save_entry(id, narration, legs, confirm)).await
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
    // Every row can be confirmed as it is, or opened.
    assert_eq!(html.matches(r#"<button type="submit">確認</button>"#).count(), 4, "{html}");
    let fee = id_of(&pool, FEE).await;
    assert!(html.contains(&format!(r#"href="/review/{fee}""#)), "{html}");
}

// --- Confirming and editing -------------------------------------------------

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

/// A new category and note overwrite the same rows; the event keeps what
/// they were.
#[tokio::test]
async fn an_edit_updates_in_place_and_keeps_the_old_state() {
    let (_dir, pool) = ledger().await;
    let fee = id_of(&pool, FEE).await;
    let before = legs(&pool, fee).await;
    let (bank, other) = (before[0].0, before[1].0);
    save(
        &pool,
        fee,
        "年費",
        vec![
            leg(Some(bank), "Assets:Bank:Savings", "-500"),
            leg(Some(other), "Expenses:Food", "500"),
        ],
        false,
    )
    .await
    .unwrap();

    let after = legs(&pool, fee).await;
    assert_eq!(after, [
        (bank, "Assets:Bank:Savings".to_string(), "-500".to_string(), None),
        (other, "Expenses:Food".to_string(), "500".to_string(), Some("manual".to_string())),
    ]);
    let narration: String = sqlx::query_scalar("SELECT narration FROM transactions WHERE id = ?")
        .bind(fee)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(narration, "年費");
    // Not confirmed: it stays in the queue.
    assert!(!reviewed(&pool, fee).await);

    let events = events(&pool, fee).await;
    let (kind, payload) = &events[0];
    assert_eq!((events.len(), kind.as_str()), (1, "edited"));
    let old = &payload["before"];
    assert_eq!(old["narration"], FEE);
    assert_eq!(old["legs"][1]["account"], "Expenses:Uncategorized");
    assert_eq!(old["legs"][1]["origin"], "fallback");
    assert_eq!(payload["after"]["legs"][1]["account"], "Expenses:Food");

    // Saved and confirmed: out of the queue, one more event.
    save(
        &pool,
        fee,
        "年費",
        vec![
            leg(Some(bank), "Assets:Bank:Savings", "-500"),
            leg(Some(other), "Expenses:Food", "500"),
        ],
        true,
    )
    .await
    .unwrap();
    assert!(reviewed(&pool, fee).await);
    let kinds: Vec<String> = events_kinds(&pool, fee).await;
    assert_eq!(kinds, ["edited", "confirmed"]);
}

async fn events_kinds(pool: &SqlitePool, id: i64) -> Vec<String> {
    events(pool, id).await.into_iter().map(|(k, _)| k).collect()
}

/// One line into several postings: the legs must still sum to zero per
/// currency, or nothing is written.
#[tokio::test]
async fn a_split_must_balance() {
    let (_dir, pool) = ledger().await;
    let taxi = id_of(&pool, TAXI).await;
    let before = legs(&pool, taxi).await;
    let (other, card) = (before[0].0, before[1].0);

    let off = save(
        &pool,
        taxi,
        TAXI,
        vec![
            leg(Some(other), "Expenses:Food", "200"),
            leg(Some(card), "Liabilities:Card", "-300"),
            leg(None, "Assets:Split:Alpha:Tab", "50"),
            // A spare row left blank is no leg.
            leg(None, "", ""),
        ],
        false,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(off.contains("借貸不平衡，差 -50 TWD"), "{off}");
    assert_eq!(legs(&pool, taxi).await, before);
    assert!(events(&pool, taxi).await.is_empty());

    save(
        &pool,
        taxi,
        TAXI,
        vec![
            leg(Some(other), "Expenses:Food", "200"),
            leg(Some(card), "Liabilities:Card", "-300"),
            leg(None, "Assets:Split:Alpha:Tab", "100"),
            leg(None, "", ""),
        ],
        true,
    )
    .await
    .unwrap();
    let after = legs(&pool, taxi).await;
    assert_eq!(after.len(), 3);
    assert_eq!((after[0].0, after[1].0), (other, card), "the old rows are kept");
    assert_eq!(after[2].1, "Assets:Split:Alpha:Tab");
    assert_eq!(events_kinds(&pool, taxi).await, ["split", "confirmed"]);
    // Still the one transaction.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE narration = ?")
        .bind(TAXI)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn an_edit_is_refused_by_name() {
    let (_dir, pool) = ledger().await;
    let fee = id_of(&pool, FEE).await;
    let rows = legs(&pool, fee).await;
    let refused =
        |legs| async { save(&pool, fee, FEE, legs, false).await.unwrap_err().to_string() };

    // Moving money the bank's statement vouches for.
    let gate = refused(vec![
        leg(Some(rows[0].0), "Assets:Bank:Savings", "-400"),
        leg(Some(rows[1].0), "Expenses:Uncategorized", "400"),
    ])
    .await;
    // One balance, in words, and what to do: not the whole check report.
    assert_eq!(
        gate.trim_start_matches("error running server function: "),
        "沒有儲存：這筆會讓「活存」2024-02-29 的餘額變成 12400 TWD，但你盤點記那天是 12300 \
         TWD。2024-02-29 以前的帳已經對過了：這筆可能已經記過，或者日期該在 2024-02-29 之後。"
    );
    let amount = refused(vec![leg(Some(rows[0].0), "Assets:Bank:Savings", "五百")]).await;
    assert!(amount.contains("金額不對：五百"), "{amount}");
    let stranger = refused(vec![
        leg(Some(rows[0].0), "Assets:Bank:Savings", "-500"),
        leg(Some(1), "Expenses:Food", "500"),
    ])
    .await;
    assert!(stranger.contains("不屬於這筆交易"), "{stranger}");
    let unknown = refused(vec![
        leg(Some(rows[0].0), "Assets:Bank:Savings", "-500"),
        leg(Some(rows[1].0), "Expenses:Nowhere", "500"),
    ])
    .await;
    assert!(unknown.contains("查無帳戶：Expenses:Nowhere"), "{unknown}");

    assert_eq!(legs(&pool, fee).await, rows);
    assert!(events(&pool, fee).await.is_empty());
}

/// The editor as the server renders it: every leg a row of plain form
/// fields, then the blank rows to split into.
#[tokio::test]
async fn the_editor_offers_each_leg_and_spare_rows() {
    let (_dir, pool) = ledger().await;
    let fee = id_of(&pool, FEE).await;
    let editing: Editing = call(&pool, || load_entry(fee)).await.unwrap();
    assert_eq!(editing.entry.legs[1].origin, Some(Origin::Fallback));
    let html = with_actions(|| view! { <Editor editing=editing.clone() /> }.to_html());
    let rows = html.matches("<tr>").count() - 1;
    assert_eq!(rows, editing.entry.legs.len() + SPARE_ROWS);
    assert!(html.contains(r#"name="legs[1][account]""#), "{html}");
    assert!(html.contains(r#"<option value="Expenses:Uncategorized" selected"#), "{html}");
    assert!(html.contains(r#"name="legs[2][currency]" value="TWD""#), "{html}");
    let text = visible(&html);
    position(&text, "未分類");
    position(&text, "同時確認");
    // No equity, no root, in what a leg may post to.
    let offered: Vec<_> = editing.accounts.iter().map(|a| a.path.as_str()).collect();
    assert!(!offered.iter().any(|p| p.starts_with("Equity") || !p.contains(':')), "{offered:?}");
}

// --- Pairing a statement line -----------------------------------------------

#[tokio::test]
async fn an_ambiguous_line_is_paired_by_a_pick() {
    let (_dir, pool) = ledger().await;
    let coffee = id_of(&pool, COFFEE).await;
    let lunch = id_of(&pool, LUNCH).await;
    let mut conn = pool.acquire().await.unwrap();
    pairing::record(&mut conn, &[Ambiguity {
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        statement_ref: "bank:line:1".into(),
        date: NaiveDate::from_ymd_opt(2024, 3, 2).unwrap(),
        amount: dec!(-150),
        description: "範例店".into(),
        candidates: vec![coffee, lunch],
    }])
    .await
    .unwrap();
    drop(conn);

    let q = queue(&pool, JournalQuery::default()).await;
    let [choice] = q.choices.as_slice() else { panic!("{:?}", q.choices) };
    let offered: Vec<_> = choice.candidates.iter().map(|e| e.id).collect();
    assert_eq!(offered, [coffee, lunch]);
    let html = with_actions(|| {
        let choices = q.choices.clone();
        view! { <web::review::Pairings choices /> }.to_html()
    });
    position(&visible(&html), "待配對");
    position(&visible(&html), "2024-03-02 活存 -150 TWD 範例店");
    assert_eq!(html.matches(r#"type="radio" name="record""#).count(), 2, "{html}");

    let id = choice.id;
    let none = call(&pool, || choose_pairing(id, None)).await.unwrap_err().to_string();
    assert!(none.contains("請選一筆紀錄"), "{none}");
    let salary = id_of(&pool, "薪水").await;
    let stranger = call(&pool, || choose_pairing(id, Some(salary))).await.unwrap_err();
    assert!(stranger.to_string().contains("不在可配對的紀錄裡"), "{stranger}");

    call(&pool, || choose_pairing(id, Some(coffee))).await.unwrap();
    let q = queue(&pool, JournalQuery::default()).await;
    assert_eq!(q.choices[0].chosen, Some(coffee));
    let chosen = with_actions(|| {
        let choices = q.choices.clone();
        view! { <web::review::Pairings choices /> }.to_html()
    });
    position(&visible(&chosen), "已選，下次匯入時對帳");
    let events = events(&pool, coffee).await;
    assert_eq!(events[0].0, "paired");
    assert_eq!(events[0].1["statement_ref"], "bank:line:1");
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

/// A figure the ledger already disagreed with holds up nothing else: a write
/// is refused only for a balance it breaks.
#[tokio::test]
async fn a_mismatch_elsewhere_does_not_block_a_write() {
    let (_dir, pool) = ledger().await;
    sqlx::query(
        "INSERT INTO balance_assertion (account_id, currency, source, period_end, closing) SELECT \
         id, 'TWD', 'statement', '2024-03-31', '1' FROM accounts WHERE path = 'Liabilities:Card'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let fee = id_of(&pool, FEE).await;
    let rows = legs(&pool, fee).await;
    save(
        &pool,
        fee,
        FEE,
        vec![
            leg(Some(rows[0].0), "Assets:Bank:Savings", "-500"),
            leg(Some(rows[1].0), "Expenses:Food", "500"),
        ],
        true,
    )
    .await
    .unwrap();
    assert!(reviewed(&pool, fee).await);
}

// --- Closing accounts -------------------------------------------------------

#[tokio::test]
async fn closing_an_account_marks_it_and_leaves_an_event() {
    let (_dir, pool) = ledger().await;
    call(&pool, || close_account("Assets:Old-Wallet".into(), Some("不用了".into()))).await.unwrap();
    let (closed, event, note): (bool, String, Option<String>) = sqlx::query_as(
        "SELECT a.closed != 0, e.event, e.note FROM accounts a JOIN account_events e ON \
         e.account_id = a.id WHERE a.path = 'Assets:Old-Wallet' ORDER BY e.id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((closed, event.as_str(), note.as_deref()), (true, "closed", Some("不用了")));

    // Gone from the pickers; still in the 帳戶 tree, marked closed.
    let journal = call(&pool, || load_journal(JournalQuery::default())).await.unwrap();
    assert!(!journal.accounts.iter().any(|a| a.path == "Assets:Old-Wallet"));
    let tree = call(&pool, load_accounts).await.unwrap();
    let assets = tree.0.iter().find(|n| n.account.path == "Assets").unwrap();
    let wallet = assets.children.iter().find(|n| n.account.path == "Assets:Old-Wallet").unwrap();
    assert!(wallet.account.closed);
    // Its history still filters by it.
    let filtered =
        call(&pool, || load_journal(JournalQuery::account("Assets:Old-Wallet"))).await.unwrap();
    assert_eq!(filtered.total, 2);
    assert!(filtered.accounts.iter().any(|a| a.path == "Assets:Old-Wallet" && a.closed));
    // Nothing new posts to it.
    let fee = id_of(&pool, FEE).await;
    let rows = legs(&pool, fee).await;
    let error = save(
        &pool,
        fee,
        FEE,
        vec![
            leg(Some(rows[0].0), "Assets:Bank:Savings", "-500"),
            leg(Some(rows[1].0), "Assets:Old-Wallet", "500"),
        ],
        false,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("帳戶已結清：Assets:Old-Wallet"), "{error}");

    call(&pool, || reopen_account("Assets:Old-Wallet".into())).await.unwrap();
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT e.event FROM account_events e JOIN accounts a ON a.id = e.account_id WHERE a.path \
         = 'Assets:Old-Wallet' ORDER BY e.id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(events, ["created", "closed", "reopened"]);
}

#[tokio::test]
async fn an_account_holding_money_is_not_closed() {
    let (_dir, pool) = ledger().await;
    let held = call(&pool, || close_account("Assets:Cash".into(), None)).await.unwrap_err();
    assert!(held.to_string().contains("餘額不是 0，不能結清：880 TWD"), "{held}");
    let parent = call(&pool, || close_account("Assets:Split".into(), None)).await.unwrap_err();
    assert!(parent.to_string().contains("底下還有 2 個帳戶沒結清"), "{parent}");
    let root = call(&pool, || close_account("Assets".into(), None)).await.unwrap_err();
    assert!(root.to_string().contains("這個帳戶不能結清"), "{root}");
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
    assert!(!queue.contains(r#"<select name="review""#), "{queue}");

    let edit = visible(&site.page("/review/6").await);
    position(&edit, "修改 · 帳簿");
    position(&edit, "手續費");
    position(&visible(&site.page("/review/999").await), "讀取失敗：查無交易 999");

    let entry = site.page("/entry").await;
    // A spend by default: an account to pay from and a category, nothing else.
    assert!(
        entry.contains(r#"name="category""#) && !entry.contains(r#"name="counter""#),
        "{entry}"
    );
    position(&visible(&entry), "支出 收入 轉帳");
    position(&visible(&entry), "付款帳戶");
    let accounts = site.page("/accounts").await;
    position(&visible(&accounts), "舊錢包 結清");
    // Every account, as a tree: the roots open, the groups under them folded.
    let assets = &accounts[position(&accounts, "資產")..];
    assert_eq!(accounts.matches(r#"<details open class="group">"#).count(), 4, "{accounts}");
    let folded = &assets[position(assets, r#"<details class="group">"#)..];
    position(&visible(folded), "分帳");
    position(&visible(&accounts), "全部展開");
    // The button says what it does.
    assert!(accounts.contains("不再使用這個帳戶"), "{accounts}");
}
