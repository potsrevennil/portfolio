//! One account's page over the sample ledger: the rendered page (layer 1)
//! and the `load_account` server function called directly (layer 2). All
//! values are invented.

mod common;

use chrono::NaiveDate;
use common::{
    page::{position, visible, Site},
    sample::{ledger, TODAY},
};
use db::assertions::{self, AssertionSource, BalanceAssertion};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use web::{
    account::{load_account, AccountView},
    model::{AccountPage, JournalQuery},
    server,
};

fn today() -> NaiveDate { TODAY.parse().unwrap() }

async fn page(pool: &SqlitePool, path: &str) -> AccountPage {
    web::detail::load(pool, &JournalQuery::account(path), today(), Currency::TWD).await.unwrap()
}

fn render(page: AccountPage) -> String {
    Owner::new().with(|| view! { <AccountView page /> }.to_html())
}

/// The `nth` table's rows as a reader sees them.
fn table(html: &str, nth: usize) -> Vec<String> {
    let body = html.split("<tbody>").nth(nth + 1).expect("the table");
    let body = &body[..body.find("</tbody>").unwrap()];
    body.split("<tr").skip(1).map(|row| visible(&format!("<tr{row}"))).collect()
}

/// A figure no import ever checked: wrong by one dollar, recorded after the
/// load's gate, as a statement would arrive.
async fn wrong_statement(pool: &SqlitePool) {
    let mut conn = pool.acquire().await.unwrap();
    assertions::insert(&mut conn, &BalanceAssertion {
        source: AssertionSource::Statement,
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        period_start: None,
        opening: None,
        period_end: "2024-02-29".parse().unwrap(),
        closing: Decimal::from(25001),
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_bank_account_shows_its_balance_month_by_month_and_its_checks() {
    let (_dir, pool) = ledger().await;
    wrong_statement(&pool).await;
    let html = render(page(&pool, "Assets:Bank:Savings").await);
    let text = visible(&html);
    assert!(text.starts_with("資產 › 銀行 › 活存 活存 25,000 TWD 去日記帳篩選 餘額走勢"), "{text}");
    // One currency: one chart, which needs no name of its own.
    assert!(!html.contains("<h3>"), "{html}");
    assert_eq!(table(&html, 0), ["2024-01 40,000 TWD", "2024-02 25,000 TWD", "2024-03 25,000 TWD"]);
    // Newest first; the opening is checked on the day before the period.
    assert_eq!(table(&html, 1), [
        "2024-03-31 對帳單 25,000 TWD 25,000 TWD ✓ 相符",
        "2024-02-29 對帳單 25,001 TWD 25,000 TWD ✗ 差 -1 TWD",
        "2024-01-01 對帳單 10,000 TWD 10,000 TWD ✓ 相符",
    ]);
    // Its journal, paged on this page.
    position(&text, "交易 共 4 筆");
    assert!(html.contains(r#"href="/journal?account=Assets%3ABank%3ASavings""#), "{html}");
}

#[tokio::test]
async fn a_group_charts_each_currency_apart_and_names_whose_check_it_is() {
    let (_dir, pool) = ledger().await;
    let html = render(page(&pool, "Assets:Bank").await);
    let text = visible(&html);
    // JPY has no rate, so the total is not a number; each currency still is.
    assert!(
        text.starts_with(
            "資產 › 銀行 銀行 未完整 （未換算：5,000.00 JPY） 25,000 TWD · 5,000.00 JPY · 90.00 \
             USD"
        ),
        "{text}"
    );
    let charts: Vec<&str> =
        html.split("<h3>").skip(1).map(|h| &h[..h.find('<').unwrap()]).collect();
    // The base currency first; the euros, zero at every month's end, draw
    // no chart.
    assert_eq!(charts, ["TWD", "JPY", "USD"]);
    // A check on a sub-account says whose it is.
    let checks = table(&html, 3);
    assert_eq!(checks[0], "2024-03-31 對帳單 活存 25,000 TWD 25,000 TWD ✓ 相符");
}

#[tokio::test]
async fn income_and_expense_show_what_moved_each_month() {
    let (_dir, pool) = ledger().await;
    let html = render(page(&pool, "Expenses:Food").await);
    let text = visible(&html);
    position(&text, "每月金額");
    // The page's total is a number when every amount has a rate.
    assert!(text.starts_with("支出 › 食食 食食 800 TWD 500 TWD · 10.00 USD"), "{text}");
    // Per currency, what each month added.
    assert!(table(&html, 0).contains(&"2024-02 0 TWD".to_string()), "{html}");
    assert!(table(&html, 1).contains(&"2024-03 10.00 USD".to_string()), "{html}");
    // No statement vouches for a category.
    assert!(!text.contains("對帳"), "{text}");

    // Income reads as money in.
    let text = visible(&render(page(&pool, "Income:Salary").await));
    assert!(text.starts_with("收入 › 薪水 薪水 30,000 TWD"), "{text}");
    assert!(text.contains("2024-01 30,000 TWD"), "{text}");
}

/// Calls the server function as the server would, with the pool in context.
async fn load(pool: &SqlitePool, query: JournalQuery) -> Result<AccountPage, ServerFnError> {
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        ScopedFuture::new(load_account(query))
    });
    call.await
}

#[tokio::test]
async fn the_server_function_names_what_it_cannot_find() {
    let (_dir, pool) = ledger().await;
    let said = |query| async { load(&pool, query).await.unwrap_err().to_string() };
    assert!(said(JournalQuery::account("Assets:Nowhere"))
        .await
        .contains("查無口座：Assets:Nowhere"));
    assert!(said(JournalQuery::default()).await.contains("沒指定口座"));
    let found = load(&pool, JournalQuery::account("Liabilities:Card")).await.unwrap();
    assert_eq!(found.label, "信用卡");
    assert_eq!(found.checks.len(), 1);
    assert!(found.checks[0].ok);
}

async fn site() -> Site {
    Site::new(|| async {
        let (dir, pool) = ledger().await;
        let options = LeptosOptions::builder().output_name("web").build();
        (server::router(options, pool, AtCost::default(), Default::default()), dir)
    })
    .await
}

#[tokio::test]
async fn the_server_renders_the_account_route_and_pages_its_journal_there() {
    let site = site().await;
    let html = site.page("/account?account=Assets%3ABank%3ASavings").await;
    // The head is sent before the account is read; the client names the tab.
    assert!(html.contains("口座 · 帳簿"), "{html}");
    position(&visible(&html), "餘額走勢");
    // The balance sheet and the charts' bars both lead here.
    let sheet = site.page("/balance-sheet").await;
    assert!(sheet.contains(r#"href="/account?account=Assets%3ABank%3ASavings""#), "{sheet}");
    let charts = site.page("/charts").await;
    assert!(charts.contains(r#"href="/account?account=Expenses%3ARent""#), "{charts}");
    let refused = visible(&site.page("/account?account=Nowhere").await);
    position(&refused, "讀取失敗：查無口座：Nowhere");
}
