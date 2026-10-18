//! 圖表 over a synthetic ledger loaded by the real load-journal: the rendered
//! charts and tables (layer 1) and the `load_charts` server function called
//! directly (layer 2). All values are invented.

mod common;

use common::{
    page::{call, position, table, visible, Site},
    sample::{ledger, today},
};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use leptos::prelude::*;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use web::{
    charts::{load_charts, ChartsView},
    model::{ChartQuery, Charts},
};

async fn charts(pool: &SqlitePool, query: ChartQuery) -> Charts {
    web::trends::load(pool, &query, today(), Currency::TWD, &AtCost::default()).await.unwrap()
}

fn render(charts: Charts) -> String {
    Owner::new().with(|| view! { <ChartsView charts /> }.to_html())
}

#[tokio::test]
async fn net_worth_is_drawn_each_month_and_says_when_it_is_incomplete() {
    let (_dir, pool) = ledger().await;
    let html = render(charts(&pool, ChartQuery::default()).await);
    let text = visible(&html);
    assert_eq!(table(&html, 0), [
        "2024-01 39,500 TWD 40,000 TWD -500 TWD",
        "2024-02 27,500 TWD 28,000 TWD -500 TWD",
        "2024-03 未完整 （未換算：5,000.00 JPY） 未完整 （未換算：5,000.00 JPY） -500 TWD",
    ]);
    // Before the pointer moves, the readout gives the newest period, and an
    // incomplete total is never shown as a number.
    let readout = &text[position(&text, "淨資產")..position(&text, "表格")];
    assert!(
        readout.starts_with("淨資產 2024-03 淨資產 未完整 （未換算：5,000.00 JPY） 資產 未完整"),
        "{readout}"
    );
    // The line stops where the figure does: two points, then a gap.
    let lines: Vec<&str> =
        html.split(r#"<path d=""#).skip(1).map(|p| &p[..p.find('"').unwrap()]).collect();
    assert_eq!(lines.len(), 3, "{html}");
    assert_eq!(lines.iter().map(|d| d.matches('L').count()).collect::<Vec<_>>(), [1, 1, 2]);
    assert_eq!(html.matches("band incomplete").count(), 1, "{html}");
}

#[tokio::test]
async fn income_and_expense_per_month_with_what_was_left() {
    let (_dir, pool) = ledger().await;
    let html = render(charts(&pool, ChartQuery::default()).await);
    assert_eq!(table(&html, 1), [
        "2024-01 30,000 TWD 500 TWD 29,500 TWD",
        "2024-02 0 TWD 12,000 TWD -12,000 TWD",
        // 10 USD at the rate of its own day.
        "2024-03 0 TWD 300 TWD -300 TWD",
    ]);
}

#[tokio::test]
async fn categories_are_ranked_with_their_part_of_the_whole() {
    let (_dir, pool) = ledger().await;
    let html = render(charts(&pool, ChartQuery::default()).await);
    let text = visible(&html);
    let spent = &text[position(&text, "支出分類")..position(&text, "收入分類")];
    assert_eq!(
        spent.trim(),
        "支出分類 支出 12,800 TWD 厝稅 12,000 TWD 94% 食食 800 TWD 6% 中晝 500 TWD 4%"
    );
    let earned = &text[position(&text, "收入分類")..];
    assert_eq!(earned.trim(), "收入分類 收入 30,000 TWD 薪水 30,000 TWD 100%");
    // 食食 folds 中晝 under it; a leaf does not fold.
    assert_eq!(html.matches(r#"<details class="group""#).count(), 1, "{html}");
}

#[tokio::test]
async fn no_account_path_or_english_shows() {
    let (_dir, pool) = ledger().await;
    let text = visible(&render(charts(&pool, ChartQuery::default()).await));
    for hidden in ["Assets", "Liabilities", "Income", "Expenses", "Equity", "權益", "Food", "Rent"]
    {
        assert!(!text.contains(hidden), "{hidden:?} shown:\n{text}");
    }
}

#[tokio::test]
async fn a_quarter_at_a_time_over_a_range() {
    let (_dir, pool) = ledger().await;
    let query = ChartQuery {
        from: Some("2024-02-01".into()),
        to: Some("2024-02-29".into()),
        step: Some("quarter".into()),
    };
    let charts = charts(&pool, query).await;
    assert_eq!(charts.worth.periods, ["2024 第1季"]);
    // Net worth is the balance on the last day, whatever the first.
    assert_eq!(charts.worth.series[0].figures[0].money.amount, Decimal::from(27500));
    // Flows count only what fell inside the range.
    assert_eq!(charts.flows.series[0].figures[0].money.amount, Decimal::ZERO);
    assert_eq!(charts.flows.series[1].figures[0].money.amount, Decimal::from(12000));
    // The served range is the one drawn, for the form to show.
    assert_eq!(charts.query.from.as_deref(), Some("2024-02-01"));
    assert_eq!(charts.query.step.as_deref(), Some("quarter"));
}

async fn load(pool: &SqlitePool, query: ChartQuery) -> Result<Charts, ServerFnError> {
    call(pool, || load_charts(query)).await
}

#[tokio::test]
async fn the_server_function_refuses_a_bad_range_by_name() {
    let (_dir, pool) = ledger().await;
    let said = |query| async { load(&pool, query).await.unwrap_err().to_string() };
    let bad_step = ChartQuery { step: Some("week".into()), ..Default::default() };
    assert!(said(bad_step).await.contains("不明的間隔：week"));
    let bad_date = ChartQuery { from: Some("2024-13-01".into()), ..Default::default() };
    assert!(said(bad_date).await.contains("日期不對：2024-13-01"));
    let backwards = ChartQuery {
        from: Some("2024-03-01".into()),
        to: Some("2024-02-01".into()),
        ..Default::default()
    };
    assert!(said(backwards).await.contains("在「到」2024-02-01 之後"));
    // Unfiltered, it runs from the ledger's first day to today.
    let whole = load(&pool, ChartQuery::default()).await.unwrap();
    assert_eq!(whole.worth.periods.first().map(String::as_str), Some("2024-01"));
}

#[tokio::test]
async fn the_server_renders_the_charts_route() {
    let site = Site::serving(ledger).await;
    let html = site.page("/charts?from=2024-01-01&to=2024-03-31&step=month").await;
    assert!(html.contains("圖表 · 帳簿"), "{html}");
    assert!(html.contains(r#"href="/charts""#), "{html}");
    let text = visible(&html);
    position(&text, "支出分類");
    position(&text, "2024-02 27,500 TWD");
    let refused = visible(&site.page("/charts?step=week").await);
    position(&refused, "讀取失敗：不明的間隔：week");
}
