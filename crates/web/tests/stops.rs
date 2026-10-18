//! 匯入卡牢: what stopped an import, as the review queue shows it and settles
//! it, over the synthetic queue ledger. The importers' side, refusing and
//! importing again, is in the import crate's `import_stops` tests. All values
//! are invented.

mod common;

use chrono::NaiveDate;
use common::{
    page::{position, visible, Site},
    queue::{ledger, COFFEE, FEE, LUNCH},
};
use db::{
    review::StatementAccounts,
    stops::{self, Record, Stop},
};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use rust_decimal_macros::dec;
use serde_json::Value;
use sqlx::SqlitePool;
use web::{
    model::{ImportStop, JournalQuery, Queue, StopKind},
    review::{load_queue, Actions},
    server,
    stops::{move_record, place_record, split_record, Stops},
};

async fn call<T, F>(pool: &SqlitePool, f: impl FnOnce() -> F) -> Result<T, ServerFnError>
where
    F: std::future::Future<Output = Result<T, ServerFnError>>,
{
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        provide_context(
            ["Assets:Bank:Savings", "Liabilities:Card"].into_iter().collect::<StatementAccounts>(),
        );
        ScopedFuture::new(f())
    });
    call.await
}

async fn queue(pool: &SqlitePool) -> Queue {
    call(pool, || load_queue(JournalQuery::default())).await.unwrap()
}

/// The stops section as the queue renders it, as text.
fn rendered(stops: &[ImportStop]) -> String {
    let stops = stops.to_vec();
    let html = Owner::new().with(|| {
        provide_context(Actions::new());
        view! { <Stops stops /> }.to_html()
    });
    visible(&html)
}

async fn id_of(pool: &SqlitePool, narration: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM transactions WHERE narration = ?")
        .bind(narration)
        .fetch_one(pool)
        .await
        .unwrap()
}

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

async fn accounts_of(pool: &SqlitePool, id: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT a.path FROM postings p JOIN accounts a ON a.id = p.account_id
         WHERE p.transaction_id = ? ORDER BY a.path",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn refuse(pool: &SqlitePool, source: &str, found: &[Stop]) {
    let mut conn = pool.acquire().await.unwrap();
    stops::record(&mut conn, source, found).await.unwrap();
}

fn day(d: &str) -> NaiveDate { d.parse().unwrap() }

/// A 天天記帳 lunch paid from 活存 that no statement line explains.
fn lunch_record(id: &str) -> Record {
    Record {
        external_ref: format!("tiantian:{id}"),
        date: day("2024-02-01"),
        account: "Assets:Bank:Savings".into(),
        amount: dec!(-500),
        currency: Currency::TWD,
        counter: vec!["Expenses:Food".into()],
        narration: "午餐".into(),
    }
}

/// The record is offered the bank line of its amount and a sibling account;
/// the pick is kept for the next import, and the line's history says so.
#[tokio::test]
async fn a_record_is_placed_on_the_line_or_account_picked() {
    let (_dir, pool) = ledger().await;
    let fee = id_of(&pool, FEE).await;
    refuse(&pool, "tiantian", &[Stop::RecordUnexplained {
        record: lunch_record("U9"),
        through: day("2024-02-29"),
        lines: vec![fee],
        accounts: vec!["Assets:Cash".into()],
    }])
    .await;

    let q = queue(&pool).await;
    let [stop] = q.stops.as_slice() else { panic!("{:?}", q.stops) };
    let StopKind::RecordUnexplained { lines, accounts, .. } = &stop.kind else {
        panic!("{stop:?}")
    };
    assert_eq!((lines[0].id, accounts[0].label.as_str()), (fee, "現金"));
    let text = rendered(&q.stops);
    position(&text, "匯入卡牢");
    position(&text, "2024-02-01 活存 -500 TWD 吃飯 · 午餐");
    position(&text, "對帳單到 2024-02-29 攏對過矣，毋過無這筆");
    position(&text, "記佇「現金」，等伊的對帳單");
    position(&text, "揀這个");

    let id = stop.id;
    let none = call(&pool, || place_record(id, None)).await.unwrap_err();
    assert!(none.to_string().contains("請揀一个"), "{none}");
    let salary = id_of(&pool, "薪水").await;
    let stranger = call(&pool, || place_record(id, Some(ImportStop::line(salary)))).await;
    assert!(stranger.unwrap_err().to_string().contains("無佇會使配的明細內底"));
    let elsewhere =
        call(&pool, || place_record(id, Some(ImportStop::account("Assets:Old-Wallet"))));
    assert!(elsewhere.await.unwrap_err().to_string().contains("袂使記佇這个帳戶"));

    call(&pool, || place_record(id, Some(ImportStop::line(fee)))).await.unwrap();
    let q = queue(&pool).await;
    assert_eq!(
        (q.stops[0].picked.clone(), q.stops[0].resolved),
        (Some(ImportStop::line(fee)), true)
    );
    position(&rendered(&q.stops), "已選，後一擺匯入就會對著");
    let history = events(&pool, fee).await;
    assert_eq!(history[0].0, "paired");
    assert_eq!(history[0].1["record_ref"], "tiantian:U9");

    // One line is one record: another record cannot take it too.
    refuse(&pool, "tiantian", &[Stop::RecordAmbiguous {
        record: lunch_record("U10"),
        lines: vec![fee],
    }])
    .await;
    let q = queue(&pool).await;
    let other = q.stops.iter().find(|s| !s.resolved).unwrap().id;
    let taken = call(&pool, || place_record(other, Some(ImportStop::line(fee)))).await.unwrap_err();
    assert!(taken.to_string().contains("已經配予另外一筆紀錄"), "{taken}");
}

/// A refusal replaces what its importer left open, but a pick survives one
/// for another reason; met again, its stop drops the pick. A run that
/// succeeds clears them all.
#[tokio::test]
async fn each_run_replaces_what_its_importer_left_open() {
    let (_dir, pool) = ledger().await;
    let fee = id_of(&pool, FEE).await;
    let picked = Stop::RecordAmbiguous { record: lunch_record("U1"), lines: vec![fee] };
    let other = Stop::RecordUnexplained {
        record: lunch_record("U2"),
        through: day("2024-02-29"),
        lines: vec![],
        accounts: vec![],
    };
    let bank =
        Stop::SeveralOpenings { account: "Assets:Bank:Savings".into(), currency: Currency::TWD };
    refuse(&pool, "tiantian", &[picked.clone(), other.clone()]).await;
    refuse(&pool, "cathay-bank", &[bank]).await;
    let id = queue(&pool).await.stops[0].id;
    call(&pool, || place_record(id, Some(ImportStop::line(fee)))).await.unwrap();

    let open = |pool: SqlitePool| async move {
        stops::open(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|o| (o.stop, o.pick.is_some()))
            .collect::<Vec<_>>()
    };
    refuse(&pool, "tiantian", std::slice::from_ref(&other)).await;
    let left = open(pool.clone()).await;
    assert_eq!(left.len(), 3, "the pick and the bank's stop stay: {left:?}");
    refuse(&pool, "tiantian", std::slice::from_ref(&picked)).await;
    let left = open(pool.clone()).await;
    assert!(left.iter().all(|(_, pick)| !pick), "{left:?}");
    assert!(!left.contains(&(other, false)), "{left:?}");

    let mut conn = pool.acquire().await.unwrap();
    stops::clear(&mut conn, "tiantian").await.unwrap();
    drop(conn);
    let left = open(pool.clone()).await;
    assert!(matches!(left.as_slice(), [(Stop::SeveralOpenings { .. }, false)]), "{left:?}");
}

/// The coffee waiting on 活存 is not on its statement; the bank shows it on
/// a sibling, and one click moves it there, as an edit.
#[tokio::test]
async fn a_record_its_statement_passes_by_moves_to_the_sibling() {
    let (_dir, pool) = ledger().await;
    let coffee = id_of(&pool, COFFEE).await;
    refuse(&pool, "cathay-bank", &[Stop::NotOnStatement {
        transaction_id: coffee,
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        date: day("2024-03-01"),
        amount: dec!(-150),
        through: day("2024-03-31"),
        siblings: vec!["Assets:Cash".into()],
    }])
    .await;
    let q = queue(&pool).await;
    let text = rendered(&q.stops);
    position(&text, "這筆猶未對帳，毋過「活存」的對帳單到 2024-03-31 攏無這筆");
    position(&text, "改記佇「現金」");
    position(&text, "修改");

    let id = q.stops[0].id;
    let wrong = call(&pool, || move_record(id, "Assets:Old-Wallet".into())).await.unwrap_err();
    assert!(wrong.to_string().contains("袂使改記佇這个帳戶"), "{wrong}");
    call(&pool, || move_record(id, "Assets:Cash".into())).await.unwrap();
    assert_eq!(accounts_of(&pool, coffee).await, ["Assets:Cash", "Expenses:Food"]);
    assert_eq!(events(&pool, coffee).await[0].0, "edited");
    position(&rendered(&queue(&pool).await.stops), "已處理，閣匯入一擺就好");
}

/// A transfer two banks dated differently is split through the clearing
/// account: each half keeps one bank's leg, and splitting twice is refused.
#[tokio::test]
async fn a_transfer_dated_twice_is_split_in_two() {
    let (_dir, pool) = ledger().await;
    let transfer = id_of(&pool, "代墊").await;
    refuse(&pool, "cathay-bank", &[Stop::TwoDates {
        transaction_id: transfer,
        dates: vec![
            ("Assets:Bank:Savings".into(), day("2024-01-05")),
            ("Assets:Split:Alpha:Tab".into(), day("2024-01-06")),
        ],
        clearing: "Assets:Bank:Clearing".into(),
    }])
    .await;
    let q = queue(&pool).await;
    let text = rendered(&q.stops);
    position(&text, "這筆轉帳兩間銀行記佇無仝日：「活存」2024-01-05、「甲公司分帳」2024-01-06");
    position(&text, "拆做兩筆");

    let id = q.stops[0].id;
    call(&pool, || split_record(id)).await.unwrap();
    assert_eq!(accounts_of(&pool, transfer).await, ["Assets:Bank:Clearing", "Assets:Bank:Savings"]);
    let detached: i64 =
        sqlx::query_scalar("SELECT max(id) FROM transactions").fetch_one(&pool).await.unwrap();
    assert_eq!(accounts_of(&pool, detached).await, [
        "Assets:Bank:Clearing",
        "Assets:Split:Alpha:Tab"
    ]);
    let history = events(&pool, detached).await;
    assert_eq!(
        (history[0].0.as_str(), history[0].1["from"].as_i64()),
        ("detached", Some(transfer))
    );
    assert_eq!(events(&pool, transfer).await[0].0, "edited");

    let again = call(&pool, || split_record(id)).await.unwrap_err();
    assert!(again.to_string().contains("已經毋是兩个帳戶之間的轉帳"), "{again}");
}

/// What only the downloads can settle is shown, with no step offered; a stop
/// about a record deleted since is left out.
#[tokio::test]
async fn stops_the_downloads_settle_are_only_shown() {
    let (_dir, pool) = ledger().await;
    let lunch = id_of(&pool, LUNCH).await;
    refuse(&pool, "cathay-bank", &[
        Stop::Unchained {
            account: "Assets:Bank:Savings".into(),
            currency: Currency::TWD,
            day: day("2024-02-10"),
            ledger: dec!(12300),
            statement: dec!(12400),
        },
        Stop::SharedDeferral {
            transaction_id: lunch,
            account: "Assets:Bank:Savings".into(),
            currency: Currency::TWD,
            date: day("2024-02-02"),
            amount: dec!(-120),
            other: "Assets:Cash".into(),
            clearing: "Assets:Bank:Clearing".into(),
        },
    ])
    .await;
    sqlx::query("UPDATE transactions SET deleted_at = 'now' WHERE id = ?")
        .bind(lunch)
        .execute(&pool)
        .await
        .unwrap();
    let q = queue(&pool).await;
    assert_eq!(q.stops.len(), 1, "{:?}", q.stops);
    let text = rendered(&q.stops);
    position(&text, "「活存」2024-02-10 的餘額，帳裡是 12,300 TWD，對帳單是 12,400 TWD");
    assert!(!text.contains("揀這个") && !text.contains("拆做兩筆"), "{text}");
}

/// The queue page carries the section above 待配對 and the journal, with
/// the server's own context: a stop recorded after the page's ledger loaded.
#[tokio::test]
async fn the_queue_page_shows_what_stopped_the_import() {
    let site = Site::new(|| async {
        let (dir, pool) = ledger().await;
        let fee = id_of(&pool, FEE).await;
        refuse(&pool, "tiantian", &[Stop::RecordAmbiguous {
            record: lunch_record("U9"),
            lines: vec![fee],
        }])
        .await;
        let options = LeptosOptions::builder().output_name("web").build();
        (server::router(options, pool, AtCost::default(), Default::default()), dir)
    })
    .await;
    let page = site.page("/review").await;
    let text = visible(&page);
    let stops = position(&text, "匯入卡牢");
    assert!(stops < position(&text, "共 4 筆"), "{text}");
    position(&text, "天天記帳這筆，對帳單有幾若筆仝款金額。是佗一筆？");
    assert_eq!(page.matches(r#"type="radio" name="place""#).count(), 1, "{page}");
}
