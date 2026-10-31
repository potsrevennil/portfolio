//! Server-side rendering of the balance sheet over a synthetic ledger: a
//! journal loaded by the real load-journal (so labels come from mapping.toml),
//! served by the real router. All values are invented.

mod common;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use common::{
    build::{self, Legs},
    page::{position, visible, Site},
};
use db::{
    assertions::{AssertionSource, BalanceAssertion},
    market::Market,
};
use ledger::{journal::Journal, valuation::AtCost};
use ledger_types::currency::Currency;
use leptos::prelude::*;
use prices::YFinanceSource;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;
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
    build::journal(txns)
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

async fn ledger() -> (TempDir, SqlitePool, Market) {
    let (dir, pool, market) = build::load(MAPPING, &journal(), &assertions()).await;
    // USD has a rate; JPY has none.
    build::usd_rate(&market, "2024-06-01", 30.0).await;
    (dir, pool, market)
}

async fn render(pool: &SqlitePool, market: &Market) -> String {
    let as_of = NaiveDate::parse_from_str(AS_OF, "%Y-%m-%d").unwrap();
    let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted:Gamma\"]\n").unwrap();
    let sheet = web::sheet::load(pool, market, as_of, Currency::TWD, &at_cost).await.unwrap();
    Owner::new().with(|| view! { <SheetView sheet /> }.to_html())
}

/// The markup as the page first shows it: a row at zero stays hidden until
/// 不顯示餘額 0 的帳戶 is unticked.
fn shown(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(i) = rest.find(r#" zero""#) {
        let start = rest[..i].rfind("<div").expect("a row at zero is a div");
        let end = i + rest[i..].find("</div>").expect("closed") + "</div>".len();
        out.push_str(&rest[..start]);
        rest = &rest[end..];
    }
    out + rest
}

#[tokio::test]
async fn labels_come_from_the_mapping_and_no_path_shows() {
    let (_dir, pool, market) = ledger().await;
    let text = visible(&shown(&render(&pool, &market).await));
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
async fn an_emptied_account_waits_for_the_toggle() {
    let (_dir, pool, market) = ledger().await;
    let html = render(&pool, &market).await;
    assert!(html.contains(r#"<div class="sheet hide-zero">"#), "{html}");
    position(&visible(&html), "不顯示餘額 0 的帳戶");
    assert!(html.contains(r#"<input type="checkbox" checked"#), "{html}");
    let row = &html[..position(&html, "Old-Wallet")];
    assert!(row[row.rfind("<div").unwrap()..].contains(r#" zero""#), "{html}");
    assert!(!visible(&shown(&html)).contains("Old-Wallet"));
}

#[tokio::test]
async fn groups_fold_in_chart_order() {
    let (_dir, pool, market) = ledger().await;
    let html = render(&pool, &market).await;
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
    // 銀行, 分帳, 甲公司 and 乙公司 fold, and so do the three sections (資產,
    // 負債, 以成本衡量之投資); leaves do not. Sections open, groups folded.
    assert_eq!(html.matches("<details").count(), 7, "{html}");
    assert_eq!(html.matches("<details open").count(), 3, "{html}");
}

#[tokio::test]
async fn every_row_is_one_figure_in_twd() {
    let (_dir, pool, market) = ledger().await;
    let page = visible(&shown(&render(&pool, &market).await));
    // Net worth heads the page, above the sections it sums; the tree follows.
    let (net, text) = net_worth_and_tree(&page);
    assert_eq!(net, "淨資產 算袂出來 （欠匯率：5,000.00 JPY）");
    // 資產: 912.34 cash + 7,000 savings + 3,000 (100 USD) + 100 splits; JPY has
    // no rate, so it is named rather than taken at 1:1, and the at-cost holding
    // and the 2025 deposit are left out.
    let assets = &text[position(text, "資產")..position(text, "銀行")];
    assert_eq!(assets.trim(), "資產 算袂出來 （欠匯率：5,000.00 JPY）");
    // A leaf holding foreign money keeps its own balance, the statement's figure.
    let fx = &text[position(text, "外幣")..position(text, "活存")];
    assert_eq!(fx.trim(), "外幣 算袂出來 （欠匯率：5,000.00 JPY） 5,000.00 JPY · 90.00 USD");
    // A group's own foreign money shows too, apart from its sub-accounts'.
    let bank = &text[position(text, "銀行")..position(text, "外幣")];
    assert_eq!(bank.trim(), "銀行 算袂出來 （欠匯率：5,000.00 JPY） 10.00 USD");
    let savings = &text[position(text, "活存")..position(text, "現金")];
    assert_eq!(savings.trim(), "活存 7,000 TWD");
    // TWD is quoted whole, as Fava prints it; the cents are still in the ledger.
    let cash = &text[position(text, "現金")..position(text, "分帳")];
    assert_eq!(cash.trim(), "現金 912 TWD");
    let split = &text[position(text, "分帳")..position(text, "甲公司")];
    assert_eq!(split.trim(), "分帳 100 TWD");
    let alpha = &text[position(text, "甲公司")..position(text, "乙公司")];
    assert_eq!(alpha.trim(), "甲公司 300 TWD 分帳 300 TWD");
    // A holding at cost is not in the tree, so no group above it shows.
    assert!(!text.contains("未上市持股"), "{text}");
    let liabilities = &text[position(text, "負債")..position(text, "信用卡")];
    assert_eq!(liabilities.trim(), "負債 -2,500 TWD");
    // Last, apart: each holding straight under the heading.
    let cost = &text[position(text, "以成本衡量之投資")..];
    assert_eq!(cost.trim(), "以成本衡量之投資 40,000 TWD 丙公司股 40,000 TWD");
}

/// The net worth row, and the tree after it.
fn net_worth_and_tree(page: &str) -> (&str, &str) {
    let net = position(page, "淨資產");
    let tree = net + "淨資產".len() + position(&page[net + "淨資產".len()..], "資產");
    (page[net..tree].trim(), &page[tree..])
}

#[tokio::test]
async fn with_every_rate_the_totals_are_whole() {
    let (_dir, pool, market) = ledger().await;
    let jpy = YFinanceSource::ticker(Currency::JPY, Currency::TWD);
    common::market::quote(&market, &jpy, "2024-06-01", 0.2).await;
    let page = visible(&render(&pool, &market).await);
    // 5,000 JPY at 0.2 adds 1,000 to every total holding it.
    let (net, text) = net_worth_and_tree(&page);
    assert_eq!(net, "淨資產 9,512 TWD");
    assert_eq!(text[..position(text, "銀行")].trim(), "資產 12,012 TWD");
    let bank = &text[position(text, "銀行")..position(text, "外幣")];
    assert_eq!(bank.trim(), "銀行 11,000 TWD 10.00 USD");
    assert!(!page.contains("算袂出來"), "{page}");
}

/// With the cache empty, every total holding foreign money says so instead
/// of passing off the TWD part as the whole.
#[tokio::test]
async fn with_no_rates_no_total_shows_a_figure() {
    let (_dir, pool, _market) = ledger().await;
    let (empty, _empty_dir) = common::market::temp().await;
    let page = visible(&render(&pool, &empty).await);
    let (net, text) = net_worth_and_tree(&page);
    assert_eq!(net, "淨資產 算袂出來 （欠匯率：5,000.00 JPY、100.00 USD）");
    assert_eq!(
        text[..position(text, "銀行")].trim(),
        "資產 算袂出來 （欠匯率：5,000.00 JPY、100.00 USD）"
    );
    // A TWD-only total still has its figure.
    let savings = &text[position(text, "活存")..position(text, "現金")];
    assert_eq!(savings.trim(), "活存 7,000 TWD");
}

async fn site() -> Site { Site::serving(ledger).await }

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
async fn shared_labels_stand_alone_with_their_parents() {
    let (_dir, pool, _market) = ledger().await;
    let data = db::query::LedgerData::load(&pool).await.unwrap();
    let labels = db::chart::standalone_labels(&data.labels());
    assert_eq!(labels["Assets:Split:Alpha:Tab"], "甲公司分帳");
    assert_eq!(labels["Assets:Split:Beta:Tab"], "乙公司分帳");
    assert_eq!(labels["Assets:Cash"], "現金");
}

#[test]
fn a_missing_mapping_lists_no_at_cost_holdings_but_a_broken_one_stops_the_server() {
    let dir = TempDir::new().unwrap();
    let missing = server::Config::load(&dir.path().join("mapping.toml")).unwrap();
    assert!(!missing.at_cost.covers("Assets:Unlisted"));

    let broken = dir.path().join("broken.toml");
    std::fs::write(&broken, "[at_cost]\naccounts = \"Assets:Unlisted\"\n").unwrap();
    assert!(server::Config::load(&broken).is_err());
}

/// A TWD asset balance as of [`AS_OF`].
fn balance(path: &str, amount: &str) -> db::query::AccountBalance {
    db::query::AccountBalance {
        account_id: 0,
        path: path.into(),
        label: String::new(),
        account_type: db::query::AccountType::Asset,
        closed: false,
        currency: Currency::TWD,
        amount: amount.parse().unwrap(),
        as_of: NaiveDate::parse_from_str(AS_OF, "%Y-%m-%d").unwrap(),
    }
}

#[test]
fn at_cost_holdings_are_listed_apart_from_every_total() {
    let as_of = NaiveDate::parse_from_str(AS_OF, "%Y-%m-%d").unwrap();
    let balances = [
        balance("Assets:Cash", "100"),
        balance("Assets:Unlisted:Gamma", "400"),
        balance("Assets:Unlisted:Delta", "-400"),
        // One holding held in two sub-accounts, with no label of its own.
        balance("Assets:Unlisted:Epsilon:Shares", "30"),
        balance("Assets:Unlisted:Epsilon:Warrants", "20"),
        // Two holdings whose leaf, their only label, is the same.
        balance("Assets:Private:Zeta", "5"),
        balance("Assets:Unlisted:Zeta", "7"),
    ];
    let at_cost =
        AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted\", \"Assets:Private\"]\n").unwrap();
    // 丙 is also a split account's label, which the tree tells apart by its
    // parent; under the heading the holding keeps its own.
    let labels = BTreeMap::from([
        ("Assets:Split", "分帳"),
        ("Assets:Split:Gamma", "丙"),
        ("Assets:Unlisted", "未上市持股"),
        ("Assets:Unlisted:Gamma", "丙"),
        ("Assets:Unlisted:Delta", "丁"),
    ]);
    let sheet = web::sheet::build(&labels, &balances, as_of, Currency::TWD, &at_cost, |_| {
        Some(Decimal::ONE)
    });
    let assets = &sheet.sections[0];
    assert_eq!(assets.total.money.amount, Decimal::from(100));
    assert!(assets.nodes.iter().all(|n| n.path == "Assets:Cash"), "{:?}", assets.nodes);
    assert_eq!(sheet.net_worth.money.amount, Decimal::from(100));
    // One row per holding, even when they cancel out; sub-accounts add up.
    let cost = sheet.at_cost.expect("the holdings at cost are listed");
    let rows: Vec<(&str, &str, Decimal)> = cost
        .nodes
        .iter()
        .map(|n| (n.path.as_str(), n.label.as_str(), n.total.money.amount))
        .collect();
    assert_eq!(rows, [
        ("Assets:Private:Zeta", "PrivateZeta", Decimal::from(5)),
        ("Assets:Unlisted:Delta", "丁", Decimal::from(-400)),
        ("Assets:Unlisted:Epsilon", "Epsilon", Decimal::from(50)),
        ("Assets:Unlisted:Gamma", "丙", Decimal::from(400)),
        ("Assets:Unlisted:Zeta", "未上市持股Zeta", Decimal::from(7)),
    ]);
    assert_eq!(cost.total.money.amount, Decimal::from(62));
}

/// An account at zero stays in the tree, marked, for the page to show on
/// request; a holding at cost at zero does not.
#[test]
fn an_account_at_zero_is_kept_and_marked() {
    let as_of = NaiveDate::parse_from_str(AS_OF, "%Y-%m-%d").unwrap();
    let balances = [
        balance("Assets:Bank:Old", "0"),
        balance("Assets:Bank:Savings", "100"),
        balance("Assets:Unlisted:Gamma", "0"),
    ];
    let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted\"]\n").unwrap();
    let sheet =
        web::sheet::build(&BTreeMap::new(), &balances, as_of, Currency::TWD, &at_cost, |_| {
            Some(Decimal::ONE)
        });
    let bank = &sheet.sections[0].nodes[0];
    let rows: Vec<(&str, bool)> = bank.children.iter().map(|n| (n.path.as_str(), n.zero)).collect();
    assert_eq!(rows, [("Assets:Bank:Old", true), ("Assets:Bank:Savings", false)]);
    assert!(!bank.zero);
    assert!(sheet.at_cost.is_none());
    assert_eq!(sheet.net_worth.money.amount, Decimal::from(100));
}
