//! 圖表 over a synthetic ledger loaded by the real load-journal: the rendered
//! charts and tables (layer 1) and the `load_charts` server function called
//! directly (layer 2). All values are invented.

mod common;

use chrono::NaiveDate;
use common::page::{position, visible, Site};
use db::{
    assertions::{AssertionSource, BalanceAssertion},
    load,
};
use ledger::{
    journal::{self, Journal, Posting},
    model::Source,
    valuation::AtCost,
};
use ledger_types::currency::Currency;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use prices::YFinanceSource;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;
use web::{
    charts::{load_charts, ChartsView},
    model::{ChartQuery, Charts},
    plot::{compact, Scale},
    server,
};

const MAPPING: &str = r#"
[accounts]
"起鼓" = "Equity:Opening-Balances"

[display]
"Assets"              = "資產"
"Assets:Bank"         = "銀行"
"Assets:Bank:Savings" = "活存"
"Assets:Bank:FX"      = "外幣"
"Liabilities"         = "負債"
"Liabilities:Card"    = "信用卡"
"Income"              = "收入"
"Income:Salary"       = "薪水"
"Expenses"            = "支出"
"Expenses:Food"       = "食食"
"Expenses:Food:Lunch" = "中晝"
"Expenses:Rent"       = "厝稅"
"Equity"              = "權益"
"#;

const TODAY: &str = "2024-03-31";

type Legs<'a> = &'a [(&'a str, &'a str, Currency)];

fn journal() -> Journal {
    use Currency::*;
    let txns: &[(&str, Legs)] = &[
        ("2024-01-01", &[
            ("Assets:Bank:Savings", "10000", TWD),
            ("Equity:Opening-Balances", "-10000", TWD),
        ]),
        ("2024-01-05", &[("Assets:Bank:Savings", "30000", TWD), ("Income:Salary", "-30000", TWD)]),
        ("2024-01-10", &[("Expenses:Food:Lunch", "500", TWD), ("Liabilities:Card", "-500", TWD)]),
        ("2024-02-01", &[("Expenses:Rent", "12000", TWD), ("Assets:Bank:Savings", "-12000", TWD)]),
        ("2024-02-03", &[
            ("Assets:Bank:FX", "100", USD),
            ("Equity:Conversions", "-100", USD),
            ("Assets:Bank:Savings", "-3000", TWD),
            ("Equity:Conversions", "3000", TWD),
        ]),
        // No JPY rate: from March on, net worth cannot be summed.
        ("2024-03-02", &[
            ("Assets:Bank:FX", "5000", JPY),
            ("Equity:Opening-Balances", "-5000", JPY),
        ]),
        // A foreign expense, converted at its own date.
        ("2024-03-05", &[("Expenses:Food", "10", USD), ("Assets:Bank:FX", "-10", USD)]),
    ];
    let postings = txns
        .iter()
        .enumerate()
        .flat_map(|(group, (date, legs))| {
            legs.iter().map(move |(account, amount, currency)| Posting {
                group: group as u64,
                source: Source::Manual,
                date: date.parse().unwrap(),
                payee: None,
                narration: "test".into(),
                external_ref: None,
                account: account.to_string(),
                amount: amount.parse::<Decimal>().unwrap(),
                currency: *currency,
                tags: None,
            })
        })
        .collect();
    Journal { postings }
}

fn assertions() -> Vec<BalanceAssertion> {
    [("Assets:Bank:Savings", "25000"), ("Liabilities:Card", "-500")]
        .into_iter()
        .map(|(account, closing)| BalanceAssertion {
            source: AssertionSource::Counted,
            account: account.into(),
            currency: Currency::TWD,
            period_start: None,
            opening: None,
            period_end: TODAY.parse().unwrap(),
            closing: closing.parse().unwrap(),
        })
        .collect()
}

async fn ledger() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("mapping.toml"), MAPPING).unwrap();
    let journal_path = root.join("journal.csv");
    journal::write(&journal_path, &journal()).unwrap();
    journal::write_assertions(&journal::assertions_path(&journal_path), &assertions()).unwrap();
    let url = format!("sqlite:{}", root.join("ledger-app.db").display());
    load::run(&load::Args {
        journal: journal_path,
        database_url: url.clone(),
        mapping: root.join("mapping.toml"),
    })
    .await
    .unwrap();
    let pool = db::init_db(&url).await.unwrap();
    let ticker = YFinanceSource::get_exchange_rate_ticker(Currency::USD, Currency::TWD).unwrap();
    sqlx::query(
        "INSERT INTO stock_prices (symbol, date, close_price) VALUES (?, '2024-01-01', 30)",
    )
    .bind(ticker)
    .execute(&pool)
    .await
    .unwrap();
    (dir, pool)
}

fn today() -> NaiveDate { TODAY.parse().unwrap() }

async fn charts(pool: &SqlitePool, query: ChartQuery) -> Charts {
    web::trends::load(pool, &query, today(), Currency::TWD, &AtCost::default()).await.unwrap()
}

fn render(charts: Charts) -> String {
    Owner::new().with(|| view! { <ChartsView charts /> }.to_html())
}

/// The `nth` table's rows as a reader sees them, one string per period.
fn table(html: &str, nth: usize) -> Vec<String> {
    let body = html.split("<tbody>").nth(nth + 1).expect("the table");
    let body = &body[..body.find("</tbody>").unwrap()];
    body.split("<tr>").skip(1).map(visible).collect()
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

/// Calls the server function as the server would, with the context in place.
async fn load(pool: &SqlitePool, query: ChartQuery) -> Result<Charts, ServerFnError> {
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        provide_context(AtCost::default());
        ScopedFuture::new(load_charts(query))
    });
    call.await
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

async fn site() -> Site {
    Site::new(|| async {
        let (dir, pool) = ledger().await;
        let options = LeptosOptions::builder().output_name("web").build();
        (server::router(options, pool, AtCost::default(), Default::default()), dir)
    })
    .await
}

#[tokio::test]
async fn the_server_renders_the_charts_route() {
    let site = site().await;
    let html = site.page("/charts?from=2024-01-01&to=2024-03-31&step=month").await;
    assert!(html.contains("圖表 · 帳簿"), "{html}");
    assert!(html.contains(r#"href="/charts""#), "{html}");
    let text = visible(&html);
    position(&text, "支出分類");
    position(&text, "2024-02 27,500 TWD");
    let refused = visible(&site.page("/charts?step=week").await);
    position(&refused, "讀取失敗：不明的間隔：week");
}

#[test]
fn axis_figures_are_compact_in_wan_and_yi() {
    let cases = [
        (0.0, "0"),
        (-0.0, "0"),
        (2500.0, "2,500"),
        (-500.0, "-500"),
        (0.5, "0.5"),
        (10000.0, "1萬"),
        (12000.0, "1.2萬"),
        (-25000.0, "-2.5萬"),
        (1234500.0, "123.5萬"),
        (150000000.0, "1.5億"),
    ];
    for (v, text) in cases {
        assert_eq!(compact(v), text, "{v}");
    }
}

#[test]
fn the_axis_takes_in_zero_on_round_steps() {
    let scale = Scale::over([39500.0, -500.0]);
    assert_eq!(scale.ticks, [-10000.0, 0.0, 10000.0, 20000.0, 30000.0, 40000.0]);
    assert_eq!(scale.top(40000.0), 0.0);
    assert_eq!(scale.top(-10000.0), 100.0);
    // Nothing to draw still makes an axis.
    assert_eq!(Scale::over([]).ticks, [0.0, 1.0]);
    let small = Scale::over([3.0, 7.5]);
    assert_eq!((small.lo, small.hi), (0.0, 8.0));
}
