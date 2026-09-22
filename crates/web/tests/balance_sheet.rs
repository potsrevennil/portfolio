//! Server-side rendering of the balance sheet over a synthetic ledger: a
//! journal loaded by the real load-journal (so labels come from mapping.toml),
//! served by the real router. All values are invented.

use axum::{body::Body, http::Request};
use chrono::NaiveDate;
use db::{
    assertions::{AssertionSource, BalanceAssertion},
    load,
};
use http_body_util::BodyExt;
use ledger::{
    journal::{self, Journal, Posting},
    model::Source,
    valuation::AtCost,
};
use ledger_types::currency::Currency;
use leptos::prelude::*;
use prices::YFinanceSource;
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
        // A group that holds money of its own, besides its sub-accounts'.
        ("2024-02-03", &[("Assets:Bank", "10", USD), ("Assets:Bank:FX", "-10", USD)]),
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
        // Cents on a TWD balance: the display rounds them, the ledger keeps them.
        ("2024-04-03", &[("Assets:Cash", "12.34", TWD), ("Income:Interest", "-12.34", TWD)]),
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

/// What the loader's gate checks this fixture against.
fn assertions() -> Vec<BalanceAssertion> {
    [("Assets:Cash", "912.34"), ("Liabilities:Card", "-2500")]
        .into_iter()
        .map(|(account, closing)| BalanceAssertion {
            source: AssertionSource::Counted,
            account: account.into(),
            currency: Currency::TWD,
            period_start: None,
            opening: None,
            period_end: AS_OF.parse().unwrap(),
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
    let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted:Gamma\"]\n").unwrap();
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
async fn every_row_is_one_figure_in_twd() {
    let (_dir, pool) = ledger().await;
    let text = visible(&render(&pool).await);
    // 資產: 912.34 cash + 7,000 savings + 3,000 (100 USD) + 100 splits; JPY has
    // no rate, so it is named rather than taken at 1:1, and the at-cost holding
    // and the 2025 deposit are left out.
    let assets = &text[position(&text, "資產")..position(&text, "銀行")];
    assert_eq!(assets.trim(), "資產 11,012 TWD （未換算：5,000.00 JPY） 另有成本 40,000 TWD");
    // A leaf holding foreign money keeps its own balance, the statement's figure.
    let fx = &text[position(&text, "外幣")..position(&text, "活存")];
    assert_eq!(fx.trim(), "外幣 2,700 TWD （未換算：5,000.00 JPY） 5,000.00 JPY · 90.00 USD");
    // A group's own foreign money shows too, apart from its sub-accounts'.
    let bank = &text[position(&text, "銀行")..position(&text, "外幣")];
    assert_eq!(bank.trim(), "銀行 10,000 TWD （未換算：5,000.00 JPY） 10.00 USD");
    let savings = &text[position(&text, "活存")..position(&text, "現金")];
    assert_eq!(savings.trim(), "活存 7,000 TWD");
    // TWD is quoted whole, as Fava prints it; the cents are still in the ledger.
    let cash = &text[position(&text, "現金")..position(&text, "分帳")];
    assert_eq!(cash.trim(), "現金 912 TWD");
    let split = &text[position(&text, "分帳")..position(&text, "甲公司")];
    assert_eq!(split.trim(), "分帳 100 TWD");
    let alpha = &text[position(&text, "甲公司")..position(&text, "乙公司")];
    assert_eq!(alpha.trim(), "甲公司 300 TWD 分帳 300 TWD");
    // A group above an at-cost holding says what its total leaves out.
    let unlisted = &text[position(&text, "未上市持股")..position(&text, "負債")];
    assert_eq!(
        unlisted.trim(),
        "未上市持股 0 TWD 另有成本 40,000 TWD 丙公司股 40,000 TWD 成本，未計入總額"
    );
    let liabilities = &text[position(&text, "負債")..position(&text, "信用卡")];
    assert_eq!(liabilities.trim(), "負債 -2,500 TWD");
    let net = &text[position(&text, "淨資產")..];
    assert_eq!(net.trim(), "淨資產 8,512 TWD （未換算：5,000.00 JPY） 另有成本 40,000 TWD");
}

/// Only one site serves at a time. Leptos keeps process-wide state, and
/// pages rendered at once on separate test runtimes can stall for good; the
/// server runs one runtime, so it never does. The global at fault is not yet
/// pinned down (separate reactive arenas did not help), so a second runtime in
/// the app, e.g. for a batch job, would need the same care.
static SERVING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The router over the fixture ledger. Tests reach pages only through it, so
/// every one takes the lock without having to know about it.
struct Site {
    router: axum::Router,
    _serving: tokio::sync::MutexGuard<'static, ()>,
    _dir: TempDir,
}

async fn site() -> Site {
    let serving = SERVING.lock().await;
    let (dir, pool) = ledger().await;
    let options = LeptosOptions::builder().output_name("web").build();
    Site { router: server::router(options, pool, AtCost::default()), _serving: serving, _dir: dir }
}

impl Site {
    async fn page(&self, path: &str) -> String {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert!(response.status().is_success(), "{path}: {}", response.status());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }
}

#[tokio::test]
async fn the_server_renders_the_balance_sheet_route() {
    let html = site().await.page("/balance-sheet").await;
    assert!(html.contains(r#"<html lang="zh-Hant-TW">"#), "{html}");
    assert!(html.contains("資產負債表 · 帳簿"), "{html}");
    let text = visible(&html);
    position(&text, "甲公司");
    position(&text, "淨資產");
}

#[tokio::test]
async fn the_landing_page_summarises() {
    let html = site().await.page("/").await;
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
    let data = db::query::LedgerData::load(&pool).await.unwrap();
    let labels = db::chart::standalone_labels(&data.labels());
    assert_eq!(labels["Assets:Split:Alpha:Tab"], "甲公司分帳");
    assert_eq!(labels["Assets:Split:Beta:Tab"], "乙公司分帳");
    assert_eq!(labels["Assets:Cash"], "現金");
}

#[test]
fn a_missing_mapping_lists_no_at_cost_holdings_but_a_broken_one_stops_the_server() {
    let dir = TempDir::new().unwrap();
    let missing = server::at_cost(&dir.path().join("mapping.toml")).unwrap();
    assert!(!missing.covers("Assets:Unlisted"));

    let broken = dir.path().join("broken.toml");
    std::fs::write(&broken, "[at_cost]\naccounts = \"Assets:Unlisted\"\n").unwrap();
    assert!(server::at_cost(&broken).is_err());
}

#[test]
fn at_cost_holdings_that_cancel_out_leave_no_note() {
    let as_of = NaiveDate::parse_from_str(AS_OF, "%Y-%m-%d").unwrap();
    let balance = |path: &str, amount: &str| db::query::AccountBalance {
        account_id: 0,
        path: path.into(),
        label: String::new(),
        account_type: db::query::AccountType::Asset,
        closed: false,
        currency: Currency::TWD,
        amount: amount.parse().unwrap(),
        as_of,
    };
    let balances = [
        balance("Assets:Cash", "100"),
        balance("Assets:Unlisted:Gamma", "400"),
        balance("Assets:Unlisted:Delta", "-400"),
    ];
    let at_cost = AtCost::parse(
        "[at_cost]\naccounts = [\"Assets:Unlisted:Gamma\", \"Assets:Unlisted:Delta\"]\n",
    )
    .unwrap();
    let sheet =
        web::sheet::build(&Default::default(), &balances, as_of, Currency::TWD, &at_cost, |_| {
            Some(Decimal::ONE)
        });
    assert_eq!(sheet.excluded, None);
    assert_eq!(sheet.sections[0].excluded, None);
    let unlisted = sheet.sections[0].nodes.iter().find(|n| n.path == "Assets:Unlisted").unwrap();
    assert_eq!(unlisted.excluded, None);
}
