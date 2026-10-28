//! 總覽 over a synthetic ledger loaded by the real load-journal: the rendered
//! page (layer 1) and the `load_charts` server function called directly
//! (layer 2). All values are invented.

mod common;

use common::{
    page::{position, printed, visible, Context, Site},
    sample::{ledger, today, MAPPING},
};
use db::market::Market;
use ledger::{investments::Investments, valuation::AtCost};
use ledger_types::currency::Currency;
use leptos::prelude::*;
use sqlx::SqlitePool;
use web::{
    charts::{load_charts, ChartsView},
    model::{ChartQuery, Charts},
    trends::Rules,
};

async fn charts(pool: &SqlitePool, market: &Market, query: ChartQuery) -> Charts {
    let (at_cost, investments) =
        (AtCost::parse(MAPPING).unwrap(), Investments::parse(MAPPING).unwrap());
    let rules = Rules { at_cost: &at_cost, investments: &investments };
    web::trends::load(pool, market, &query, today(), Currency::TWD, rules).await.unwrap()
}

fn render(charts: Charts) -> String {
    Owner::new().with(|| view! { <ChartsView charts /> }.to_html())
}

/// The text of the legend of the `nth` donut on the page.
fn legend(html: &str, nth: usize) -> String {
    let list = html.split(r#"<ul class="legend-list">"#).nth(nth + 1).expect("the legend");
    visible(&list[..list.find("</ul>").unwrap()])
}

fn whole() -> ChartQuery { ChartQuery::period(ChartQuery::WHOLE) }

#[tokio::test]
async fn net_worth_is_one_line_that_says_when_it_is_incomplete() {
    let (_dir, pool, market) = ledger().await;
    let charts = charts(&pool, &market, ChartQuery::default()).await;
    assert_eq!(charts.worth.series.len(), 1);
    assert_eq!(printed(&charts.worth.series[0]), [
        "39,500 TWD",
        "25,500 TWD",
        "未完整（未換算：5,000.00 JPY）",
    ]);
    // Its arms and its change are read out beside it, not drawn.
    let noted: Vec<&str> = charts.worth.noted.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(noted, ["資產", "負債", "變化"]);
    assert_eq!(printed(&charts.worth.noted[2]), [
        "39,500 TWD",
        "-14,000 TWD",
        "未完整（未換算：5,000.00 JPY）",
    ]);
    // Held at cost, apart from net worth.
    assert_eq!(charts.at_cost.as_ref().map(|c| c.money.to_string()).as_deref(), Some("4,000 TWD"));
    let html = render(charts);
    let text = visible(&html);
    position(&text, "以成本衡量之投資 4,000 TWD ，不計入淨資產");
    // Before the pointer moves, the readout gives the newest figures.
    let readout = &text[position(&text, "至今")..position(&text, "1萬")];
    assert!(
        readout.starts_with(
            "至今 淨資產 未完整 （未換算：5,000.00 JPY） 資產 未完整 （未換算：5,000.00 JPY） \
             負債 -500 TWD 變化 未完整 （未換算：5,000.00 JPY）"
        ),
        "{readout}"
    );
    // The line stops where the figure does: two points, then a gap.
    let lines: Vec<&str> = html
        .split(r#"<path d=""#)
        .skip(1)
        .filter(|p| p[..p.find('>').unwrap()].contains(r#"class="line"#))
        .map(|p| &p[..p.find('"').unwrap()])
        .collect();
    assert_eq!(lines.len(), 1, "{html}");
    assert_eq!(lines[0].matches('L').count(), 1, "{}", lines[0]);
    assert_eq!(html.matches("band incomplete").count(), 1, "{html}");
}

#[tokio::test]
async fn investment_results_stand_apart_from_income_and_expense() {
    let (_dir, pool, market) = ledger().await;
    let charts = charts(&pool, &market, ChartQuery::default()).await;
    let [income, expense] = &charts.flows.series[..] else { panic!("two series") };
    assert_eq!(printed(income), ["30,000 TWD", "0 TWD", "0 TWD"]);
    // 10 USD at the rate of its own day; the investment loss is not spending.
    assert_eq!(printed(expense), ["500 TWD", "12,000 TWD", "300 TWD"]);
    assert_eq!(printed(&charts.flows.noted[0]), ["29,500 TWD", "-12,000 TWD", "-300 TWD"]);
    // Before a month is picked, each flow reads as its sum.
    let summary = charts.flows.summary.as_ref().expect("a flow has a sum");
    assert_eq!(summary.name, "合計");
    let totals: Vec<String> = summary.figures.iter().map(|f| f.money.to_string()).collect();
    assert_eq!(totals, ["30,000 TWD", "12,800 TWD", "17,200 TWD"]);
    let investments = charts.investments.expect("the sample has investment results");
    assert_eq!(printed(&investments.series[0]), ["0 TWD", "-2,000 TWD", "500 TWD"]);
    let total = &investments.summary.expect("a flow has a sum").figures[0];
    assert_eq!(total.money.to_string(), "-1,500 TWD");
}

#[tokio::test]
async fn a_month_is_broken_down_by_category_with_its_part_of_the_whole() {
    let (_dir, pool, market) = ledger().await;
    // Unasked, the newest month with day-to-day flows.
    let html = render(charts(&pool, &market, ChartQuery::default()).await);
    let text = visible(&html);
    // Every year to pick, 全部 first, then the months of the year shown.
    let picker = &text[position(&text, "收支分類")..position(&text, "也可以")];
    assert_eq!(picker.trim(), "收支分類 2024 年 3 月 全部 2024 1月 2月 3月");
    assert!(html.contains(r#"aria-current="true" href="/?period=2024-03-01""#), "{html}");
    assert_eq!(legend(&html, 0), "食食 300 TWD 100%");
    assert_eq!(legend(&html, 1), "");
    // The month broken down is marked on 收支, which also opens each.
    assert_eq!(html.matches("band selected").count(), 1, "{html}");
    assert!(html.contains(r#"href="/?period=2024-02-01""#), "{html}");

    let html = render(charts(&pool, &market, whole()).await);
    position(&visible(&html), "收支分類 全部期間");
    assert_eq!(legend(&html, 0), "厝稅 12,000 TWD 94% 食食 800 TWD 6%");
    assert_eq!(legend(&html, 1), "薪水 30,000 TWD 100%");
    // Two slices and a whole ring drawn, each in a colour of its own.
    assert_eq!(html.matches(r#"class="slot s"#).count(), 3, "{html}");

    // A year covers each of its months, all marked.
    let html = render(charts(&pool, &market, ChartQuery::period("2024")).await);
    position(&visible(&html), "收支分類 2024 年");
    assert_eq!(legend(&html, 0), "厝稅 12,000 TWD 94% 食食 800 TWD 6%");
    assert_eq!(html.matches("band selected").count(), 3, "{html}");
}

#[tokio::test]
async fn no_account_path_or_english_shows() {
    let (_dir, pool, market) = ledger().await;
    for query in [ChartQuery::default(), whole()] {
        let text = visible(&render(charts(&pool, &market, query).await));
        for hidden in ["Assets", "Liabilities", "Income", "Expenses", "Equity", "權益", "Food"] {
            assert!(!text.contains(hidden), "{hidden:?} shown:\n{text}");
        }
    }
}

#[tokio::test]
async fn the_page_leads_with_where_things_stand() {
    let (_dir, pool, market) = ledger().await;
    let text = visible(&render(charts(&pool, &market, ChartQuery::default()).await));
    let headline = &text[..position(&text, "以成本")];
    // Net worth and its change in the newest month, the newest month with
    // day-to-day flows, and this year's investment result.
    assert!(
        headline.starts_with(
            "總覽 2024-03-31 淨資產 未完整 （未換算：5,000.00 JPY） 較上月 未完整 \
             （未換算：5,000.00 JPY） 3 月收入 0 TWD 3 月支出 300 TWD 3 月結餘 -300 TWD 2024 \
             投資損益 -1,500 TWD 淨資產"
        ),
        "{headline}"
    );
}

async fn load(
    pool: &SqlitePool,
    market: &Market,
    query: ChartQuery,
) -> Result<Charts, ServerFnError> {
    Context::from(pool).market(market).call(|| load_charts(query)).await
}

#[tokio::test]
async fn the_server_function_refuses_a_period_it_does_not_have() {
    let (_dir, pool, market) = ledger().await;
    let said = |query| async { load(&pool, &market, query).await.unwrap_err().to_string() };
    assert!(said(ChartQuery::period("2024-02-15")).await.contains("不明的期間：2024-02-15"));
    assert!(said(ChartQuery::period("someday")).await.contains("不明的期間：someday"));
    // Unasked, it runs from the ledger's first day to today.
    let whole = load(&pool, &market, ChartQuery::default()).await.unwrap();
    assert_eq!(whole.worth.periods.first().map(String::as_str), Some("2024-01"));
}

#[tokio::test]
async fn the_server_renders_the_landing_page() {
    let site = Site::serving(ledger).await;
    let html = site.page("/").await;
    // The tree lives on its own page, reached from the nav.
    assert!(html.contains("總覽 · 帳簿"), "{html}");
    assert!(html.contains(r#"href="/balance-sheet""#), "{html}");
    position(&visible(&html), "收支分類 2024 年 3 月");
    let refused = visible(&site.page("/?period=someday").await);
    position(&refused, "讀取失敗：不明的期間：someday");
}
