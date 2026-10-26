//! What stopped the last imports, as the review queue lists it, over the
//! synthetic queue ledger. The importers' side is in the import crate's
//! `import_stops` tests. All values are invented.

mod common;

use common::{
    call::queue,
    page::{position, visible, Site},
    queue::{id_of, ledger, COFFEE, FEE, LUNCH},
    stops::{lunch, refuse, unexplained},
};
use db::stops::Stop;
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use leptos::prelude::*;
use rust_decimal_macros::dec;
use web::{model::JournalQuery, server, stops::Stops};

/// Each kind of stop says what is wrong and what to fix, naming its records;
/// one about a record deleted since is left out.
#[tokio::test]
async fn each_stop_is_listed_with_its_fix() {
    let (_dir, pool) = ledger().await;
    let (fee, coffee, lunch_id) =
        (id_of(&pool, FEE).await, id_of(&pool, COFFEE).await, id_of(&pool, LUNCH).await);
    let day = |d: &str| d.parse().unwrap();
    refuse(&pool, "tiantian", &[
        Stop::RecordAmbiguous { record: lunch("U1"), lines: vec![fee] },
        unexplained("U2", vec![fee]),
    ])
    .await;
    refuse(&pool, "cathay-bank", &[
        Stop::NotOnStatement {
            transaction_id: coffee,
            account: "Assets:Bank:Savings".into(),
            currency: Currency::TWD,
            date: day("2024-03-01"),
            amount: dec!(-150),
            through: day("2024-03-31"),
            siblings: vec!["Assets:Cash".into()],
        },
        Stop::SharedDeferral {
            transaction_id: lunch_id,
            account: "Assets:Bank:Savings".into(),
            currency: Currency::TWD,
            date: day("2024-02-02"),
            amount: dec!(-120),
            other: "Assets:Cash".into(),
        },
        Stop::TwoDates {
            transaction_id: id_of(&pool, "代墊").await,
            dates: vec![
                ("Assets:Bank:Savings".into(), day("2024-01-05")),
                ("Assets:Split:Alpha:Tab".into(), day("2024-01-06")),
            ],
        },
        Stop::SeveralOpenings { account: "Assets:Bank:Savings".into(), currency: Currency::TWD },
        Stop::Unchained {
            account: "Assets:Bank:Savings".into(),
            currency: Currency::TWD,
            day: day("2024-02-10"),
            ledger: dec!(12300),
            statement: dec!(12400),
        },
    ])
    .await;
    sqlx::query("UPDATE transactions SET deleted_at = 'now' WHERE id = ?")
        .bind(lunch_id)
        .execute(&pool)
        .await
        .unwrap();

    let stops = queue(&pool, JournalQuery::default()).await.stops;
    assert_eq!(stops.len(), 6, "the shared deferral's record is gone: {stops:?}");
    let html = Owner::new().with(|| view! { <Stops stops /> }.to_html());
    let text = visible(&html);
    for said in [
        "2024-02-01 活存 -500 TWD 吃飯 · 午餐",
        "分袂出是佗一筆",
        "對帳單到 2024-02-29 無這筆",
        "「活存」對帳單到 2024-03-31 無這筆，「現金」有仝款",
        "兩間銀行紀錄無仝工：「活存」2024-01-05、「甲公司分帳」2024-01-06",
        "「活存」有兩筆以上期初",
        "帳裡 12,300 TWD，對帳單 12,400 TWD",
    ] {
        position(&text, said);
    }
    assert!(html.contains(&format!(r#"href="/review/{coffee}""#)), "{html}");
}

/// The list sits above 待配對 and the journal on the queue page.
#[tokio::test]
async fn the_queue_page_lists_what_stopped_the_import() {
    let site = Site::new(|| async {
        let (dir, pool) = ledger().await;
        refuse(&pool, "tiantian", &[unexplained("U9", vec![])]).await;
        let options = LeptosOptions::builder().output_name("web").build();
        let market = common::market::beside(dir.path()).await;
        (server::router(options, pool, market, AtCost::default(), Default::default()), dir)
    })
    .await;
    let text = visible(&site.page("/review").await);
    assert!(position(&text, "無這筆") < position(&text, "共 4 筆"), "{text}");
}
