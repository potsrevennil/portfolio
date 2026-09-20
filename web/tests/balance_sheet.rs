//! Server-side rendering of the balance sheet over a synthetic ledger: a
//! journal loaded by the real load-journal (so labels come from mapping.toml),
//! served by the real router. All values are invented.

use axum::{body::Body, http::Request};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use leptos::prelude::*;
use portfolio::{
    currency::Currency,
    db,
    ledger::{
        journal::{self, Journal, Posting},
        load,
        valuation::AtCost,
    },
    YFinanceSource,
};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use web::{balance_sheet::SheetView, server};

const MAPPING: &str = r#"
[accounts]
"現金" = "Assets:Cash"
"起鼓" = "Equity:Opening-Balances"

[expenses]
"食食" = "Expenses:Food"

[display]
"Assets"                  = "資產"
"Assets:Bank"             = "銀行"
"Assets:Bank:Savings"     = "活存"
"Assets:Bank:FX"          = "外幣"
"Assets:Split"            = "分帳"
"Assets:Split:Alpha"      = "甲公司"
"Assets:Split:Alpha:Tab"  = "分帳"
"Assets:Split:Beta"       = "乙公司"
"Assets:Split:Beta:Tab"   = "分帳"
"Assets:Unlisted"         = "未上市持股"
"Assets:Unlisted:Gamma"   = "丙公司股"
"Liabilities"             = "負債"
"Liabilities:Card"        = "信用卡"
"Equity"                  = "權益"
"#;

const AS_OF: &str = "2024-12-31";

/// Legs of one transaction: (account, amount, currency).
type Legs<'a> = &'a [(&'a str, &'a str, Currency)];

fn journal() -> Journal {
    use Currency::*;
    let txns: &[(&str, Legs)] = &[
        ("2024-01-01", &[("Assets:Cash", "1000", TWD), ("Equity:Opening-Balances", "-1000", TWD)]),
        ("2024-01-01", &[
            ("Assets:Bank:Savings", "50000", TWD),
            ("Equity:Opening-Balances", "-50000", TWD),
        ]),
        // One account, two foreign currencies.
        ("2024-02-01", &[
            ("Assets:Bank:FX", "100", USD),
            ("Equity:Conversions", "-100", USD),
            ("Assets:Bank:Savings", "-3000", TWD),
            ("Equity:Conversions", "3000", TWD),
        ]),
        ("2024-02-02", &[
            ("Assets:Bank:FX", "5000", JPY),
            ("Equity:Opening-Balances", "-5000", JPY),
        ]),
        ("2024-03-01", &[
            ("Assets:Split:Alpha:Tab", "300", TWD),
            ("Assets:Split:Beta:Tab", "-200", TWD),
            ("Assets:Cash", "-100", TWD),
        ]),
        ("2024-04-01", &[("Expenses:Food", "2500", TWD), ("Liabilities:Card", "-2500", TWD)]),
        // Held at cost: shown, but in no total.
        ("2024-04-02", &[
            ("Assets:Unlisted:Gamma", "40000", TWD),
            ("Assets:Bank:Savings", "-40000", TWD),
        ]),
        // Closed out to zero: must not show.
        ("2024-05-01", &[("Assets:Old-Wallet", "70", TWD), ("Assets:Cash", "-70", TWD)]),
        ("2024-05-02", &[("Assets:Old-Wallet", "-70", TWD), ("Assets:Cash", "70", TWD)]),
        // After the as-of date: must not count.
        ("2025-01-05", &[("Assets:Cash", "999", TWD), ("Equity:Opening-Balances", "-999", TWD)]),
    ];
    let postings = txns
        .iter()
        .enumerate()
        .flat_map(|(group, (date, legs))| {
            legs.iter().map(move |(account, amount, currency)| Posting {
                group: group as u64,
                source: "manual".into(),
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

async fn ledger() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("mapping.toml"), MAPPING).unwrap();
    journal::write(root.join("journal.csv"), &journal()).unwrap();
    let url = format!("sqlite:{}", root.join("ledger-app.db").display());
    load::run(&load::Args {
        journal: root.join("journal.csv"),
        database_url: url.clone(),
        mapping: root.join("mapping.toml"),
    })
    .await
    .unwrap();
    let pool = db::init_db(&url).await.unwrap();
    let ticker = YFinanceSource::get_exchange_rate_ticker(Currency::USD, Currency::TWD).unwrap();
    sqlx::query(
        "INSERT INTO stock_prices (symbol, date, close_price) VALUES (?, '2024-06-01', 30)",
    )
    .bind(ticker)
    .execute(&pool)
    .await
    .unwrap();
    (dir, pool)
}

async fn render(pool: &SqlitePool) -> String {
    let as_of = NaiveDate::parse_from_str(AS_OF, "%Y-%m-%d").unwrap();
    let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted\"]\n").unwrap();
    let sheet = web::sheet::load(pool, as_of, Currency::TWD, &at_cost).await.unwrap();
    Owner::new().with(|| view! { <SheetView sheet /> }.to_html())
}

/// The text a reader sees: markup and scripts removed.
fn visible(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = match rest.starts_with("<script") {
            true => rest.find("</script>").map_or(rest.len(), |i| i + "</script>".len()),
            false => rest.find('>').map_or(rest.len(), |i| i + 1),
        };
        out.push(' ');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn position(text: &str, needle: &str) -> usize {
    text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in:\n{text}"))
}

#[tokio::test]
async fn labels_come_from_the_mapping_and_no_path_shows() {
    let (_dir, pool) = ledger().await;
    let text = visible(&render(&pool).await);
    for label in [
        "資產",
        "負債",
        "現金",
        "銀行",
        "活存",
        "外幣",
        "分帳",
        "甲公司",
        "乙公司",
        "信用卡",
        "淨資產",
    ] {
        position(&text, label);
    }
    for hidden in ["Assets", "Liabilities", "Equity", "權益", "起鼓", "Old-Wallet", "Split", "Bank"]
    {
        assert!(!text.contains(hidden), "{hidden:?} shown:\n{text}");
    }
}

#[tokio::test]
async fn groups_fold_in_chart_order() {
    let (_dir, pool) = ledger().await;
    let html = render(&pool).await;
    let text = visible(&html);
    // Children follow their group, groups in chart (path) order: Bank, Cash,
    // Split. In the tree a shared label stands bare; its group says whose it is.
    assert!(!text.contains("甲公司分帳"), "{text}");
    let mut rest = text.as_str();
    for label in [
        "資產",
        "銀行",
        "外幣",
        "活存",
        "現金",
        "分帳",
        "甲公司",
        "分帳",
        "乙公司",
        "分帳",
        "負債",
        "信用卡",
    ] {
        rest = &rest[position(rest, label) + label.len()..];
    }
    // 銀行, 分帳, 甲公司, 乙公司 and 未上市持股 fold, and so do the two sections;
    // leaves do not. Sections are rendered open, groups folded.
    assert_eq!(html.matches("<details").count(), 7, "{html}");
    assert_eq!(html.matches("<details open").count(), 2, "{html}");
}

#[tokio::test]
async fn totals_are_per_currency_and_converted() {
    let (_dir, pool) = ledger().await;
    let text = visible(&render(&pool).await);
    // 外幣 holds two currencies on one row; JPY has no rate and is named, not
    // converted at 1:1.
    let fx = &text[position(&text, "外幣")..position(&text, "活存")];
    assert_eq!(fx.trim(), "外幣 5,000 JPY 100 USD ≈ 3,000 TWD （未換算：JPY）");
    let savings = &text[position(&text, "活存")..position(&text, "現金")];
    assert_eq!(savings.trim(), "活存 7,000 TWD");
    // 資產: 1,000 − 100 cash + 47,000 + 3,000 (USD) + 300 − 200 splits; the
    // 2025 deposit is after the date.
    let assets = &text[position(&text, "資產")..position(&text, "銀行")];
    assert!(assets.contains("5,000 JPY 8,000 TWD 100 USD ≈ 11,000 TWD"), "{assets}");
    // The at-cost holding is named apart from the total, not added to it.
    assert!(assets.contains("另有成本 40,000 TWD"), "{assets}");
    let unlisted = &text[position(&text, "未上市持股")..position(&text, "負債")];
    assert!(unlisted.contains("40,000 TWD 成本，未計入總額"), "{unlisted}");
    let split = &text[position(&text, "分帳")..position(&text, "甲公司")];
    assert_eq!(split.trim(), "分帳 100 TWD");
    let alpha = &text[position(&text, "甲公司")..position(&text, "乙公司")];
    assert_eq!(alpha.trim(), "甲公司 300 TWD 分帳 300 TWD");
    // A section holding only the base currency needs no converted line.
    let liabilities = &text[position(&text, "負債")..position(&text, "信用卡")];
    assert_eq!(liabilities.trim(), "負債 -2,500 TWD");
    let net = &text[position(&text, "淨資產")..];
    assert!(net.starts_with("淨資產 8,500 TWD （未換算：JPY） 另有成本 40,000 TWD"), "{net}");
}

#[tokio::test]
async fn the_server_renders_the_balance_sheet_route() {
    let (_dir, pool) = ledger().await;
    let options = LeptosOptions::builder().output_name("web").build();
    let response = server::router(options, pool, AtCost::default())
        .oneshot(Request::get("/balance-sheet").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let html = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec())
        .unwrap();
    assert!(html.contains(r#"<html lang="zh-Hant-TW">"#), "{html}");
    assert!(html.contains("資產負債表 · 帳簿"), "{html}");
    let text = visible(&html);
    position(&text, "甲公司");
    position(&text, "淨資產");
}

#[tokio::test]
async fn the_landing_page_summarises() {
    let (_dir, pool) = ledger().await;
    let options = LeptosOptions::builder().output_name("web").build();
    let response = server::router(options, pool, AtCost::default())
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let html = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec())
        .unwrap();
    assert!(html.contains("總覽 · 帳簿"), "{html}");
    let text = visible(&html);
    position(&text, "淨資產");
    position(&text, "資產");
    position(&text, "負債");
    // The tree itself lives on its own page, reached from the nav.
    assert!(html.contains(r#"href="/balance-sheet""#), "{html}");
    assert!(!text.contains("全部展開"), "{text}");
}

#[tokio::test]
async fn shared_labels_stand_alone_with_their_parents() {
    let (_dir, pool) = ledger().await;
    let labels = portfolio::store::chart::standalone_labels(
        &portfolio::store::chart::accounts(&pool).await.unwrap(),
    );
    assert_eq!(labels["Assets:Split:Alpha:Tab"], "甲公司分帳");
    assert_eq!(labels["Assets:Split:Beta:Tab"], "乙公司分帳");
    assert_eq!(labels["Assets:Cash"], "現金");
}
