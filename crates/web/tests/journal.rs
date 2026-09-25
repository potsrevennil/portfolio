//! The journal over a synthetic ledger loaded by the real load-journal: the
//! rendered list (layer 1) and the `load_journal` server function called
//! directly (layer 2). All values are invented.

mod common;

use common::page::{position, visible, Site};
use db::{
    assertions::{AssertionSource, BalanceAssertion},
    load,
};
use ledger::{
    journal::{self, Journal, Posting, UNVERIFIED_TAG},
    model::Source,
    valuation::AtCost,
};
use ledger_types::currency::Currency;
use leptos::{prelude::*, reactive::computed::ScopedFuture};
use leptos_router::params::ParamsMap;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;
use web::{
    journal::{load_journal, JournalFilter, JournalList},
    model::{AccountChoice, AccountKind, Journal as Page, JournalQuery, Review},
    server,
};

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
"Liabilities"             = "負債"
"Liabilities:Card"        = "信用卡"
"Income"                  = "收入"
# 銀行 names an account under two roots, as a chart does when the same thing
# comes in and goes out: neither is under a parent that could tell them apart.
"Income:Bank"             = "銀行"
"Income:Bank:Interest"    = "利息"
"Expenses"                = "支出"
"Equity"                  = "權益"
"Equity:Conversions"      = "匯兌"
"#;

const DINNER: &str = "<b>Tom & Jerry's</b>";
const JUICE: &str = "100% 果汁";
/// Records older than the rest, enough to fill a second page.
const FILLER: usize = 60;

struct Txn {
    date: String,
    payee: Option<&'static str>,
    narration: String,
    unverified: bool,
    legs: Vec<(&'static str, &'static str, Currency)>,
}

fn txn(date: &str, narration: &str, legs: &[(&'static str, &'static str, Currency)]) -> Txn {
    Txn {
        date: date.into(),
        payee: None,
        narration: narration.into(),
        unverified: false,
        legs: legs.to_vec(),
    }
}

fn journal() -> Journal {
    use Currency::*;
    let mut txns = vec![
        txn("2024-01-01", "期初", &[
            ("Assets:Bank:Savings", "50000", TWD),
            ("Equity:Opening-Balances", "-50000", TWD),
        ]),
        txn("2024-01-01", "期初", &[
            ("Assets:Cash", "1000", TWD),
            ("Equity:Opening-Balances", "-1000", TWD),
        ]),
        Txn {
            payee: Some("換匯"),
            ..txn("2024-02-01", "美金", &[
                ("Assets:Bank:FX", "100", USD),
                ("Equity:Conversions", "-100", USD),
                ("Assets:Bank:Savings", "-3000", TWD),
                ("Equity:Conversions", "3000", TWD),
            ])
        },
        txn("2024-03-01", "分帳", &[
            ("Assets:Split:Alpha:Tab", "300", TWD),
            ("Assets:Split:Beta:Tab", "-200", TWD),
            ("Assets:Cash", "-100", TWD),
        ]),
        Txn {
            payee: Some(DINNER),
            ..txn("2024-04-01", "晚餐 <script>alert(1)</script>", &[
                ("Expenses:Food", "2500", TWD),
                ("Liabilities:Card", "-2500", TWD),
            ])
        },
        // Cents on TWD: the display rounds them, the ledger keeps them.
        txn("2024-04-03", "利息", &[
            ("Assets:Cash", "12.34", TWD),
            ("Income:Bank:Interest", "-12.34", TWD),
        ]),
        Txn {
            unverified: true,
            ..txn("2024-05-10", JUICE, &[
                ("Expenses:Food", "250.5", TWD),
                ("Assets:Bank:Savings", "-250.5", TWD),
            ])
        },
    ];
    for i in 0..FILLER {
        let date =
            chrono::NaiveDate::from_ymd_opt(2023, 1, 1).unwrap() + chrono::Days::new(i as u64);
        txns.push(txn(&date.to_string(), &format!("零食 {i}"), &[
            ("Expenses:Food", "10", TWD),
            ("Liabilities:Card", "-10", TWD),
        ]));
    }
    let postings = txns
        .iter()
        .enumerate()
        .flat_map(|(group, t)| {
            t.legs.iter().map(move |(account, amount, currency)| Posting {
                group: group as u64,
                source: Source::Manual,
                date: t.date.parse().unwrap(),
                payee: t.payee.map(Into::into),
                narration: t.narration.clone(),
                external_ref: None,
                account: account.to_string(),
                amount: amount.parse::<Decimal>().unwrap(),
                currency: *currency,
                tags: t.unverified.then(|| UNVERIFIED_TAG.to_string()),
            })
        })
        .collect();
    Journal { postings }
}

fn assertions() -> Vec<BalanceAssertion> {
    [("Assets:Cash", "912.34"), ("Liabilities:Card", "-3100")]
        .into_iter()
        .map(|(account, closing)| BalanceAssertion {
            source: AssertionSource::Counted,
            account: account.into(),
            currency: Currency::TWD,
            period_start: None,
            opening: None,
            period_end: "2024-12-31".parse().unwrap(),
            closing: closing.parse().unwrap(),
        })
        .collect()
}

/// The dinner and the juice await review.
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
    sqlx::query("UPDATE transactions SET reviewed = 0 WHERE payee = ? OR narration = ?")
        .bind(DINNER)
        .bind(JUICE)
        .execute(&pool)
        .await
        .unwrap();
    (dir, pool)
}

/// Calls the server function as the server would, with the pool in context.
async fn load(pool: &SqlitePool, query: JournalQuery) -> Result<Page, ServerFnError> {
    let owner = Owner::new();
    let call = owner.with(|| {
        provide_context(pool.clone());
        ScopedFuture::new(load_journal(query))
    });
    call.await
}

async fn journal_page(pool: &SqlitePool, query: JournalQuery) -> Page {
    load(pool, query).await.unwrap()
}

fn render(journal: Page) -> String {
    Owner::new().with(|| view! { <JournalList action="/journal" journal /> }.to_html())
}

fn render_filter(
    query: JournalQuery,
    chosen: Option<String>,
    accounts: Vec<AccountChoice>,
) -> String {
    Owner::new()
        .with(|| view! { <JournalFilter action="/journal" query chosen accounts /> }.to_html())
}

/// The names the account picker offers, in the order it offers them.
fn offered(html: &str) -> Vec<String> {
    let list = &html[position(html, "<datalist")..];
    list[..position(list, "</datalist>")]
        .split(r#"<option value=""#)
        .skip(1)
        .map(|option| option.split('"').next().unwrap().to_string())
        .collect()
}

/// Each entry's visible text, newest first.
fn entries(html: &str) -> Vec<String> {
    html.split(r#"<li class="entry"#)
        .skip(1)
        .map(|entry| {
            let entry = &entry[entry.find('>').unwrap() + 1..];
            visible(entry.split("</ol>").next().unwrap())
        })
        .collect()
}

fn narrations(journal: &Page) -> Vec<String> {
    journal.entries.iter().map(|e| e.narration.clone().unwrap_or_default()).collect()
}

async fn site() -> Site {
    Site::new(|| async {
        let (dir, pool) = ledger().await;
        let options = LeptosOptions::builder().output_name("web").build();
        (server::router(options, pool, AtCost::default()), dir)
    })
    .await
}

// --- Layer 1: the rendered list --------------------------------------------

#[tokio::test]
async fn newest_first_with_labels_and_rounded_twd() {
    let (_dir, pool) = ledger().await;
    let html = render(journal_page(&pool, JournalQuery::default()).await);
    let entries = entries(&html);
    // TWD is quoted whole, half away from zero; the ledger keeps the cents.
    assert_eq!(entries[0], "2024-05-10 100% 果汁 未對帳 未確認 食食 251 TWD 活存 -251 TWD");
    assert_eq!(entries[1], "2024-04-03 利息 現金 12 TWD 利息 -12 TWD");
    // A shared label stands with its parent's in front: no tree shows it here.
    assert_eq!(entries[3], "2024-03-01 分帳 甲公司分帳 300 TWD 乙公司分帳 -200 TWD 現金 -100 TWD");
    // Foreign legs keep two places; the conversion plug shows as 匯兌, as in Fava.
    assert_eq!(
        entries[4],
        "2024-02-01 換匯 美金 外幣 100.00 USD 匯兌 -100.00 USD 活存 -3,000 TWD 匯兌 3,000 TWD"
    );
    assert_eq!(entries[5], "2024-01-01 期初 現金 1,000 TWD 起鼓 -1,000 TWD");
    let text = visible(&html);
    for path in ["Assets", "Equity", "Expenses", "Split", "Savings", "Conversions"] {
        assert!(!text.contains(path), "{path:?} shown:\n{text}");
    }
}

#[tokio::test]
async fn payee_and_narration_are_escaped() {
    let (_dir, pool) = ledger().await;
    let html = render(journal_page(&pool, JournalQuery::default()).await);
    assert!(html.contains("&lt;b&gt;Tom &amp; Jerry's&lt;/b&gt;"), "{html}");
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{html}");
    assert!(!html.contains("<script>alert"), "{html}");
    assert!(!html.contains("<b>Tom"), "{html}");
}

#[tokio::test]
async fn unverified_records_stand_out() {
    let (_dir, pool) = ledger().await;
    let html = render(journal_page(&pool, JournalQuery::default()).await);
    assert_eq!(html.matches(r#"class="entry unverified"#).count(), 1, "{html}");
    assert_eq!(visible(&html).matches("未對帳").count(), 1);
    // Unreviewed is a separate state: the dinner has only that badge.
    let dinner = entries(&html).into_iter().find(|e| e.contains("晚餐")).unwrap();
    assert!(dinner.contains("未確認") && !dinner.contains("未對帳"), "{dinner}");
}

#[tokio::test]
async fn pages_hold_fifty_and_link_to_their_neighbours() {
    let (_dir, pool) = ledger().await;
    let first = render(journal_page(&pool, JournalQuery::default()).await);
    assert_eq!(entries(&first).len(), 50);
    position(&visible(&first), "共 67 筆 · 第 1／2 頁");
    assert!(first.contains(r#"href="/journal?page=2" rel="next""#), "{first}");
    assert!(!first.contains(r#"rel="prev""#), "{first}");

    // Past the last page is the last page; the filter rides along.
    let query = JournalQuery { text: Some("零食".into()), page: 9, ..Default::default() };
    let last = render(journal_page(&pool, query).await);
    position(&visible(&last), "共 60 筆 · 第 2／2 頁");
    assert_eq!(entries(&last).len(), 10);
    assert!(last.contains(r#"href="/journal?q=%E9%9B%B6%E9%A3%9F" rel="prev""#), "{last}");
    // The oldest comes last.
    assert!(entries(&last).last().unwrap().starts_with("2023-01-01"));
}

#[tokio::test]
async fn filtered_legs_are_marked_and_every_leg_links_to_its_account() {
    let (_dir, pool) = ledger().await;
    let html = render(journal_page(&pool, JournalQuery::account("Assets:Split")).await);
    position(&visible(&html), "分帳 共 1 筆");
    assert_eq!(html.matches(r#"class="leg focus""#).count(), 2, "{html}");
    assert!(html.contains(r#"href="/journal?account=Assets%3ASplit%3AAlpha%3ATab""#), "{html}");
}

// --- Layer 2: the server function ------------------------------------------

#[tokio::test]
async fn an_account_filters_to_its_subtree() {
    let (_dir, pool) = ledger().await;
    let bank = journal_page(&pool, JournalQuery::account("Assets:Bank")).await;
    assert_eq!(narrations(&bank), [JUICE, "美金", "期初"]);
    assert_eq!(bank.account.as_deref(), Some("銀行"));
    // A path prefix that is not an account of its own names none: it is
    // refused, rather than listing the subtree it looks like.
    let partial = load(&pool, JournalQuery::account("Assets:Ba")).await.unwrap_err().to_string();
    assert!(partial.contains("查無帳戶：Assets:Ba"), "{partial}");
    let leaf = journal_page(&pool, JournalQuery::account("Assets:Bank:FX")).await;
    assert_eq!(narrations(&leaf), ["美金"]);
    let focus: Vec<_> = leaf.entries[0].legs.iter().map(|l| (l.label.as_str(), l.focus)).collect();
    assert_eq!(focus, [("外幣", true), ("匯兌", false), ("活存", false), ("匯兌", false)]);
}

#[tokio::test]
async fn a_typed_name_and_a_linked_path_reach_the_same_account() {
    let (_dir, pool) = ledger().await;
    // The name the picker offers, the bare label, and the path a link carries.
    for typed in ["甲公司分帳（資產）", "甲公司分帳", "Assets:Split:Alpha:Tab"] {
        let page = journal_page(&pool, JournalQuery::account(typed)).await;
        assert_eq!(page.query.account.as_deref(), Some("Assets:Split:Alpha:Tab"), "{typed}");
        assert_eq!(page.account.as_deref(), Some("甲公司分帳"), "{typed}");
    }
    // A root names its whole section.
    let assets = journal_page(&pool, JournalQuery::account("資產")).await;
    assert_eq!(assets.query.account.as_deref(), Some("Assets"));
}

#[tokio::test]
async fn a_name_no_one_account_answers_to_is_refused_by_name() {
    let (_dir, pool) = ledger().await;
    let refused =
        async |name: &str| load(&pool, JournalQuery::account(name)).await.unwrap_err().to_string();
    assert!(refused("活存2").await.contains("查無帳戶：活存2"));
    // 銀行 stands under two roots: the picker's own names tell them apart, so
    // the refusal offers them rather than falling back to every account.
    let clash = refused("銀行").await;
    assert!(clash.contains("帳戶名重複：銀行（收入）、銀行（資產）"), "{clash}");
    for (typed, path) in [("銀行（資產）", "Assets:Bank"), ("銀行（收入）", "Income:Bank")]
    {
        let page = journal_page(&pool, JournalQuery::account(typed)).await;
        assert_eq!(page.query.account.as_deref(), Some(path), "{typed}");
    }
    // An empty box is not a refusal: it is every account.
    assert_eq!(journal_page(&pool, JournalQuery::default()).await.total, 67);
}

#[tokio::test]
async fn dates_bound_the_range_inclusively() {
    let (_dir, pool) = ledger().await;
    let query = JournalQuery {
        from: Some("2024-03-01".into()),
        to: Some("2024-04-03".into()),
        ..Default::default()
    };
    let range = journal_page(&pool, query).await;
    assert_eq!(narrations(&range), ["利息", "晚餐 <script>alert(1)</script>", "分帳"]);

    let bad = JournalQuery { from: Some("2024-13-01".into()), ..Default::default() };
    let error = load(&pool, bad).await.unwrap_err().to_string();
    assert!(error.contains("日期不對：2024-13-01"), "{error}");
}

#[tokio::test]
async fn text_matches_payee_or_narration_literally() {
    let (_dir, pool) = ledger().await;
    let search = |text: &str| JournalQuery { text: Some(text.into()), ..Default::default() };
    assert_eq!(narrations(&journal_page(&pool, search("果汁")).await), [JUICE]);
    // Payee, any ASCII case.
    assert_eq!(journal_page(&pool, search("tom &")).await.total, 1);
    // LIKE's wildcards are only characters here.
    assert_eq!(narrations(&journal_page(&pool, search("%")).await), [JUICE]);
    assert_eq!(journal_page(&pool, search("_")).await.total, 0);
}

#[tokio::test]
async fn review_and_verification_are_separate_filters() {
    let (_dir, pool) = ledger().await;
    let review = |review| JournalQuery::default().with_review(review);
    let unreviewed = journal_page(&pool, review(Review::Unreviewed)).await;
    assert_eq!(narrations(&unreviewed), [JUICE, "晚餐 <script>alert(1)</script>"]);
    assert_eq!(journal_page(&pool, review(Review::Reviewed)).await.total, 65);

    let unverified = JournalQuery { unverified: true, ..Default::default() };
    let unverified = journal_page(&pool, unverified).await;
    assert_eq!(narrations(&unverified), [JUICE]);
    assert!(unverified.entries[0].unverified && !unverified.entries[0].reviewed);

    // A state the app never writes is refused by name, as a bad date is.
    let bad = JournalQuery { review: Some("unreviwed".into()), ..Default::default() };
    let error = load(&pool, bad).await.unwrap_err().to_string();
    assert!(error.contains("不明的確認狀態：unreviwed"), "{error}");
}

#[tokio::test]
async fn the_picker_offers_every_account_by_kind_and_no_equity_one() {
    let (_dir, pool) = ledger().await;
    let journal = journal_page(&pool, JournalQuery::default()).await;
    let html = render_filter(journal.query.clone(), journal.chosen.clone(), journal.accounts);
    // Sections in balance-sheet order, paths within; a root is its own kind,
    // so it is not named twice over.
    assert_eq!(offered(&html), [
        "資產",
        "銀行（資產）",
        "外幣（資產）",
        "活存（資產）",
        "現金（資產）",
        "分帳（資產）",
        "甲公司（資產）",
        "甲公司分帳（資產）",
        "乙公司（資產）",
        "乙公司分帳（資產）",
        "負債",
        "信用卡（負債）",
        "收入",
        "銀行（收入）",
        "利息（收入）",
        "支出",
        "食食（支出）",
    ]);
    // 權益 is the loader's own; nobody files under it.
    assert!(!html.contains("權益") && !html.contains("起鼓") && !html.contains("匯兌"), "{html}");
    // An empty box is every account, and says so.
    assert!(html.contains(r#"placeholder="全部""#), "{html}");
    assert!(html.contains(r#"value="""#), "{html}");
}

#[tokio::test]
async fn the_chosen_account_survives_the_round_trip() {
    let (_dir, pool) = ledger().await;
    // A leg's link carries the path; the box comes back holding the name.
    let linked = journal_page(&pool, JournalQuery::account("Assets:Split:Alpha:Tab")).await;
    assert_eq!(linked.chosen.as_deref(), Some("甲公司分帳（資產）"));
    let html = render_filter(linked.query.clone(), linked.chosen.clone(), linked.accounts.clone());
    assert!(html.contains(r#"value="甲公司分帳（資產）""#), "{html}");

    // Submitting that name again lands on the same account, and the links the
    // page leaves behind still carry the path.
    let typed = journal_page(&pool, JournalQuery::account("甲公司分帳（資產）")).await;
    assert_eq!(typed.query.account.as_deref(), Some("Assets:Split:Alpha:Tab"));
    assert_eq!(typed.chosen, linked.chosen);
    assert_eq!(narrations(&typed), narrations(&linked));
}

#[test]
fn an_account_name_is_escaped_where_the_picker_offers_it() {
    let label = r#"<b>甲 & "乙"</b>"#;
    let choice = AccountChoice { label: label.into(), kind: AccountKind::Expense };
    let chosen = Some(choice.to_string());
    let html = render_filter(JournalQuery::default(), chosen, vec![choice]);
    assert!(!html.contains("<b>甲"), "{html}");
    assert_eq!(html.matches("&lt;b&gt;").count(), 2, "{html}");
    assert!(html.contains("&amp;"), "{html}");
    assert!(!html.contains(r#""乙""#), "{html}");
}

#[test]
fn a_query_survives_the_url() {
    let query = JournalQuery {
        account: Some("Assets:Bank:Savings".into()),
        from: Some("2024-01-01".into()),
        to: None,
        text: Some("晚餐 & 100%".into()),
        review: Some(Review::Unreviewed.to_string()),
        unverified: true,
        page: 3,
    };
    let url = query.to_string();
    let params: ParamsMap =
        form_urlencoded::parse(url.trim_start_matches('?').as_bytes()).into_owned().collect();
    assert_eq!(JournalQuery::from(&params), query);
    assert_eq!(JournalQuery::default().to_string(), "");
    // A submitted form sends every field, the blank ones empty.
    let blank: ParamsMap = [("account", ""), ("from", ""), ("q", " "), ("review", "")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(JournalQuery::from(&blank), JournalQuery::default());
}

// --- Through the router ----------------------------------------------------

#[tokio::test]
async fn a_refused_filter_is_reported_in_the_reader_s_words() {
    let site = site().await;
    let text = visible(&site.page("/journal?review=unreviwed").await);
    position(&text, "讀取失敗：不明的確認狀態：unreviwed");
    // The framework's English about the call itself is not the reader's business.
    assert!(!text.contains("server function"), "{text}");
}

#[tokio::test]
async fn the_journal_route_renders_with_its_filter() {
    let site = site().await;
    let html = site.page("/journal?account=Assets%3ASplit&review=unreviewed").await;
    assert!(html.contains("日記帳 · 帳簿"), "{html}");
    // The box holds the name; the list the browser filters holds every other.
    assert!(html.contains(r#"value="分帳（資產）""#), "{html}");
    assert!(offered(&html).contains(&"甲公司分帳（資產）".to_string()), "{html}");
    assert!(!html.contains("起鼓"), "{html}");
    let text = visible(&html);
    position(&text, "查無交易");
}

#[tokio::test]
async fn balance_sheet_accounts_open_their_journal() {
    let html = site().await.page("/balance-sheet").await;
    assert!(html.contains(r#"href="/journal?account=Assets%3ABank%3ASavings""#), "{html}");
    assert!(html.contains(r#"href="/journal?account=Assets%3ASplit%3AAlpha%3ATab""#), "{html}");
}
