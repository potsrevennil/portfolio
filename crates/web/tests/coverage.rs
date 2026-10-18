//! T07's 對帳進度 over a synthetic ledger loaded by the real load-journal: the
//! builder on a fixed day, the server function and route as served, and the
//! rendered rows. All values are invented.

mod common;

use chrono::NaiveDate;
use common::{
    coverage::{ledger, Ledger, TODAY},
    page::{position, router, visible, Site},
};
use leptos::prelude::*;
use web::{
    coverage::{
        load_coverage, Coverage, CoverageView, Files, Gap, Missing, SourceRow, Status, Tail,
    },
    progress,
};

fn today() -> NaiveDate { TODAY.parse().unwrap() }

async fn coverage() -> Coverage {
    let l = ledger().await;
    progress::load(&l.pool, &l.config, today()).await.unwrap()
}

fn render(coverage: Coverage) -> String {
    Owner::new().with(|| view! { <CoverageView coverage /> }.to_html())
}

fn source<'a>(c: &'a Coverage, account: &str) -> &'a SourceRow {
    c.sources.iter().find(|s| s.account == account).unwrap_or_else(|| panic!("{account}: {c:#?}"))
}

#[tokio::test]
async fn sources_are_listed_in_config_order_with_their_group() {
    let c = coverage().await;
    let labels: Vec<_> = c.sources.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["銀行 活存", "銀行 外幣", "甲卡", "現金", "券商 甲券商", "預付"]);
}

#[tokio::test]
async fn records_after_the_last_statement_are_imported_not_checked() {
    let c = coverage().await;
    assert_eq!(source(&c, "Assets:Bank:Savings"), &SourceRow {
        label: "銀行 活存".into(),
        account: "Assets:Bank:Savings".into(),
        cadence: "隨時下載".into(),
        status: Status::Unchecked,
        checked: Some("2024-02-29".into()),
        per_commodity: vec![],
        behind_days: Some(10),
        imported: Some("2024-03-05".into()),
        tail: Some(Tail {
            count: 1,
            first: "2024-03-05".into(),
            last: "2024-03-05".into(),
            link: Some(
                "/journal?account=Assets%3ABank%3ASavings&from=2024-03-05&unverified=1".into()
            ),
        }),
        gaps: vec![Gap {
            commodity: "TWD".into(),
            from: "2024-02-01".into(),
            to: "2024-02-09".into()
        }],
        missing: None,
        files: None,
    });
}

#[tokio::test]
async fn an_account_is_checked_only_as_far_as_its_least_checked_currency() {
    let c = coverage().await;
    let fx = source(&c, "Assets:Bank:FX");
    // EUR closed at zero and has not moved since, so it holds nothing back.
    assert_eq!(fx.checked.as_deref(), Some("2023-12-31"));
    assert_eq!(fx.per_commodity, ["JPY 2023-12-31", "USD 2024-02-29"]);
    assert_eq!(fx.behind_days, Some(70));
    assert_eq!(fx.status, Status::Checked);
    assert_eq!(fx.tail, None);
}

#[tokio::test]
async fn one_counted_account_does_not_vouch_for_its_sibling() {
    let c = coverage().await;
    let prepaid = source(&c, "Assets:Prepaid");
    assert_eq!(prepaid.checked, None);
    assert_eq!(prepaid.per_commodity, ["禮券 TWD 從未", "月票 TWD 2024-02-01"]);
    assert_eq!(prepaid.tail.as_ref().map(|t| t.count), Some(1));
    assert_eq!(prepaid.status, Status::Overdue);
}

#[tokio::test]
async fn a_card_is_late_from_the_first_statement_it_missed() {
    let c = coverage().await;
    let card = source(&c, "Liabilities:Card");
    assert_eq!(card.cadence, "每月 25 日結帳");
    assert_eq!(card.status, Status::Overdue);
    assert_eq!(card.checked.as_deref(), Some("2023-12-25"));
    assert_eq!(
        card.missing,
        Some(Missing {
            from: Some("2024-01-25".into()),
            through: Some("2024-02-25".into()),
            count: false,
            url: Some("https://card.example/statements".into()),
        })
    );
    // The liability leg is not marked: the tail is every record after the
    // statement, and the link does not narrow to marked ones.
    assert_eq!(
        card.tail.as_ref().and_then(|t| t.link.as_deref()),
        Some("/journal?account=Liabilities%3ACard&from=2024-01-20")
    );
    // Only its own folder: `raw/cards/` is another.
    assert_eq!(
        card.files,
        Some(Files {
            count: 2,
            latest: "raw/card/2024-01.csv".into(),
            downloaded: Some("2024-02-03".into()),
        })
    );
}

#[tokio::test]
async fn cash_is_checked_by_a_count_not_by_tiantian() {
    let c = coverage().await;
    let cash = source(&c, "Assets:Cash");
    // 天天記帳's own closing on 02-29 is that app's sum, not a count.
    assert_eq!(cash.checked.as_deref(), Some("2024-01-31"));
    assert_eq!(cash.status, Status::ToCount);
    assert_eq!(cash.missing.as_ref().and_then(|m| m.from.as_deref()), Some("2024-03-01"));
    assert!(cash.missing.as_ref().is_some_and(|m| m.count));
}

#[tokio::test]
async fn a_broker_is_held_to_its_own_records() {
    let c = coverage().await;
    let broker = source(&c, "Assets:Broker:Alpha");
    // The money sent in (a posting) is not the broker's record; its deposit
    // and purchase are, and the quarter's statement covers both.
    assert_eq!(broker.imported.as_deref(), Some("2023-12-01"));
    assert_eq!(broker.checked.as_deref(), Some("2023-12-31"));
    assert_eq!(broker.status, Status::Checked);
    assert_eq!(broker.tail, None);
    assert_eq!(broker.cadence, "每季");
}

#[tokio::test]
async fn a_balance_no_source_covers_is_named() {
    let c = coverage().await;
    let unsourced: Vec<_> =
        c.unsourced.iter().map(|u| (u.label.as_str(), u.account.as_str())).collect();
    assert_eq!(unsourced, [("分帳 甲公司", "Assets:Split:Alpha")]);
}

#[tokio::test]
async fn the_page_shows_the_tail_apart_from_the_checked_day() {
    let c = coverage().await;
    let html = render(c);
    let text = visible(&html);
    let row =
        |from: &str, to: &str| text[position(&text, from)..position(&text, to)].trim().to_string();
    assert_eq!(
        row("銀行 活存", "銀行 外幣"),
        "銀行 活存 隨時下載 已匯入未對帳 2024-02-29 10 日 2024-03-05 1 筆，2024-03-05 TWD \
         2024-02-01 → 2024-02-09"
    );
    assert_eq!(
        row("銀行 外幣", "甲卡"),
        "銀行 外幣 隨時下載 已對帳 2023-12-31 JPY 2023-12-31 · USD 2024-02-29 70 日 2024-01-15"
    );
    assert_eq!(
        row("甲卡", "現金"),
        "甲卡 每月 25 日結帳 逾期 2023-12-25 76 日 2024-01-20 1 筆，2024-01-20 2024-01-25 → \
         2024-02-25 的對帳單 去下載 2 個檔，上新 2024-01.csv，2024-02-03 下載"
    );
    assert_eq!(
        row("現金", "券商 甲券商"),
        "現金 每 30 日點算 待點算 2024-01-31 39 日 2024-02-02 1 筆，2024-02-02 2024-03-01 就該點算"
    );
    // The tail links to the journal; the unverified badge is styled apart.
    assert!(html.contains(r#"href="/journal?account=Assets%3ABank%3ASavings&amp;from=2024-03-05&amp;unverified=1""#), "{html}");
    assert!(html.contains(r#"class="badge status unchecked""#), "{html}");
    assert!(html.contains(r#"href="https://card.example/statements""#), "{html}");
    let unsourced = &text[position(&text, "無設定來源的帳戶")..];
    assert!(unsourced.contains("分帳 甲公司 記到 2024-01-05"), "{unsourced}");
}

/// Runs a server function as the server would, with the ledger's context.
async fn call<T, F>(l: &Ledger, f: impl FnOnce() -> F) -> Result<T, ServerFnError>
where
    F: std::future::Future<Output = Result<T, ServerFnError>>,
{
    let provide = || {
        provide_context(l.pool.clone());
        provide_context(l.config.clone());
    };
    common::page::call(provide, f).await
}

#[tokio::test]
async fn the_server_function_reads_the_configured_sources() {
    let l = ledger().await;
    let c = call(&l, load_coverage).await.unwrap();
    assert_eq!(c.today, chrono::Local::now().date_naive().to_string());
    assert_eq!(c.sources.len(), 6);
    // Long past every due day of the fixture, whatever today is.
    assert_eq!(source(&c, "Liabilities:Card").status, Status::Overdue);
    assert_eq!(source(&c, "Assets:Bank:Savings").status, Status::Unchecked);
}

#[tokio::test]
async fn the_route_renders_and_is_in_the_nav() {
    let site = Site::new(|| async {
        let l = ledger().await;
        (router(l.pool, Default::default(), l.config), l.dir)
    })
    .await;
    let html = site.page("/coverage").await;
    assert!(html.contains("對帳進度 · 帳簿"), "{html}");
    assert!(html.contains(r#"href="/coverage""#), "{html}");
    let text = visible(&html);
    position(&text, "已對帳至");
    position(&text, "銀行 活存");
}

#[tokio::test]
async fn a_ledger_without_sources_says_so() {
    let l = ledger().await;
    let config = progress::Config::default();
    let c = progress::load(&l.pool, &config, today()).await.unwrap();
    assert!(c.sources.is_empty());
    // Every balance is then one nobody checks.
    assert!(c.unsourced.iter().any(|u| u.account == "Assets:Bank:Savings"), "{c:#?}");
    let html = render(c);
    assert!(visible(&html).contains("[[sources]]"), "{html}");
}
