//! T07's 對帳進度 over a synthetic ledger loaded by the real load-journal: the
//! builder on a fixed day, the server functions and route as served, and the
//! rendered bars. All values are invented.

mod common;

use chrono::NaiveDate;
use common::{
    coverage::{ledger, splits, Ledger, TODAY},
    page::{position, visible, Context, Site},
};
use ledger_types::{
    assertion::{AssertionSource, BalanceAssertion},
    cadence::{Cadence, CadenceKind, Standing},
    currency::Currency,
};
use leptos::prelude::*;
use rust_decimal_macros::dec;
use web::{
    coverage::{
        build, confirm_balance, coverage_due, load_coverage, save_schedule, AccountRow,
        CadenceName, Coverage, CoverageActions, CoverageView, GapSpan, Mark,
    },
    error::said,
    model::account_href,
    server,
};

fn today() -> NaiveDate { TODAY.parse().unwrap() }

async fn read(l: &Ledger) -> Coverage {
    build::load(&l.pool, today(), &splits(), Currency::TWD).await.unwrap()
}

async fn coverage() -> Coverage { read(&ledger().await).await }

/// Due or late, and holding something: what the nav's badge counts.
fn reminded(c: &Coverage) -> impl Iterator<Item = &AccountRow> {
    c.accounts.iter().filter(|a| !a.zero() && a.status.is_due())
}

fn render(coverage: Coverage) -> String {
    Owner::new().with(|| {
        provide_context(CoverageActions::new());
        view! { <CoverageView coverage /> }.to_html()
    })
}

fn account<'a>(c: &'a Coverage, path: &str) -> &'a AccountRow {
    c.accounts.iter().find(|a| a.account == path).unwrap_or_else(|| panic!("{path}: {c:#?}"))
}

fn mark(date: &str, ago: i64) -> Option<Mark> { Some(Mark { date: date.into(), ago }) }

/// Savings' two statements sit nine days apart on one balance, so nothing moved
/// unseen. A third opening on another balance has money between it and the
/// last.
#[tokio::test]
async fn days_no_statement_covers_show_where_the_balance_moved() {
    let l = ledger().await;
    let c = read(&l).await;
    assert_eq!(account(&c, "Assets:Bank:Savings").gaps, []);

    let conn = &mut *l.pool.acquire().await.unwrap();
    let later = BalanceAssertion {
        source: AssertionSource::Statement,
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        period_start: Some("2024-03-05".parse().unwrap()),
        opening: Some(dec!(10000)),
        period_end: "2024-03-10".parse().unwrap(),
        closing: dec!(10000),
    };
    db::assertions::insert(conn, &later).await.unwrap();

    let c = read(&l).await;
    let row = account(&c, "Assets:Bank:Savings");
    let (from, to) = ("2024-03-01", "2024-03-04");
    let ago = |d: &str| (today() - d.parse::<NaiveDate>().unwrap()).num_days();
    assert_eq!(row.gaps, [GapSpan {
        from: Mark { date: from.into(), ago: ago(from) },
        to: Mark { date: to.into(), ago: ago(to) },
    }]);
    let page = render(c);
    assert!(page.contains("2024-03-01 到 2024-03-04 無數單"), "{page}");
    assert!(page.contains(r#"class="gap""#), "{page}");
}

#[tokio::test]
async fn accounts_are_grouped_by_kind_and_name_their_bank() {
    let c = coverage().await;
    let rows: Vec<_> =
        c.accounts.iter().map(|a| (a.group.as_str(), a.label.as_str(), a.bank.as_str())).collect();
    // In the chart's order, instalments last. A set bank first; otherwise the
    // parent's label, or the account's own straight under a root.
    assert_eq!(rows, [
        ("銀行", "外幣", "銀行"),
        ("銀行", "活存", "甲銀行"),
        ("券商", "甲券商", "券商"),
        ("現金", "現金", "現金"),
        ("舊錢包", "舊錢包", "舊錢包"),
        ("預付", "禮券", "預付"),
        ("預付", "月票", "預付"),
        ("分帳", "甲公司", "分帳"),
        ("信用卡", "信用卡", "甲銀行"),
        ("分期", "閱讀器", "分期"),
    ]);
}

#[tokio::test]
async fn a_bank_account_checked_through_its_closing_is_current() {
    let c = coverage().await;
    let savings = account(&c, "Assets:Bank:Savings");
    assert_eq!(CadenceName(savings.cadence).to_string(), "逐月數單");
    assert_eq!(savings.status, Standing::Current);
    assert_eq!(savings.wanted, None);
    assert_eq!(savings.checked, mark("2024-02-29", 10));
    assert_eq!(savings.recorded, mark("2024-03-05", 5));
    assert_eq!(savings.first, mark("2023-11-01", 130));
    // Recorded past the statement, apart from the green.
    let tail = savings.tail.as_ref().unwrap();
    assert_eq!(tail.count, 1);
    assert_eq!(
        tail.link.as_deref(),
        Some("/journal?account=Assets%3ABank%3ASavings&from=2024-03-05&unverified=1")
    );
}

#[tokio::test]
async fn a_dormant_currency_does_not_hold_the_account_back() {
    let c = coverage().await;
    let fx = account(&c, "Assets:Bank:FX");
    // JPY's last statement still holds: nothing moved it since.
    assert_eq!(fx.checked, mark("2024-02-29", 10));
    assert!(fx.per_commodity.is_empty());
    // Unscheduled: never due.
    assert_eq!(fx.status, Standing::Unscheduled);
    assert_eq!(CadenceName(fx.cadence).to_string(), "無固定數單");
}

#[tokio::test]
async fn instalments_show_what_is_paid_and_left() {
    let c = coverage().await;
    let kobo = account(&c, "Liabilities:Instalments:Kobo");
    assert_eq!(CadenceName(kobo.cadence).to_string(), "分期付款");
    assert_eq!(kobo.status, Standing::Unscheduled);
    let i = kobo.instalment.as_ref().unwrap();
    assert_eq!(
        (i.paid, i.total, i.first.as_str(), i.last.as_str()),
        (2, 12, "2024-01-10", "2024-11-20")
    );
}

#[tokio::test]
async fn a_split_account_shows_what_is_owed_and_since_when() {
    let c = coverage().await;
    let alpha = account(&c, "Assets:Split:Alpha").split.as_ref().unwrap();
    assert_eq!(alpha.owed.iter().map(ToString::to_string).collect::<Vec<_>>(), ["200 TWD"]);
    assert_eq!(alpha.since.as_ref().map(|m| m.date.as_str()), Some("2024-01-05"));
    assert_eq!(account(&c, "Assets:Bank:Savings").split, None);
}

#[tokio::test]
async fn a_card_is_late_from_the_first_statement_it_missed() {
    let c = coverage().await;
    let card = account(&c, "Liabilities:Card");
    assert_eq!(card.status, Standing::Overdue);
    assert_eq!(card.wanted.as_deref(), Some("1–2 月的數單"));
    assert_eq!(card.late_days, Some(45));
}

#[tokio::test]
async fn cash_is_checked_by_a_count_not_by_tiantian() {
    let c = coverage().await;
    let cash = account(&c, "Assets:Cash");
    // 天天記帳's own closing on 02-29 is that app's sum, not a count.
    assert_eq!(cash.checked, mark("2024-01-31", 39));
    assert_eq!(cash.status, Standing::Due);
    assert_eq!(cash.wanted, None);
}

#[tokio::test]
async fn a_broker_is_held_to_its_own_records() {
    let c = coverage().await;
    let broker = account(&c, "Assets:Broker:Alpha");
    // The money sent in (a posting) is not the broker's record; its deposit
    // and purchase are, and the quarter's statement covers both.
    assert_eq!(broker.recorded, mark("2023-12-01", 100));
    assert_eq!(broker.status, Standing::Current);
    assert_eq!(broker.tail, None);
}

#[tokio::test]
async fn one_counted_account_does_not_vouch_for_its_sibling() {
    let c = coverage().await;
    assert_eq!(account(&c, "Assets:Prepaid:Pass").checked, mark("2024-02-01", 38));
    assert_eq!(account(&c, "Assets:Prepaid:Gift").checked, None);
}

#[tokio::test]
async fn reminders_list_what_is_due_on_accounts_holding_something() {
    let c = coverage().await;
    assert!(account(&c, "Assets:Old").zero());
    let due: Vec<_> = reminded(&c).map(|a| a.account.as_str()).collect();
    assert_eq!(due, ["Assets:Cash", "Liabilities:Card"]);
}

#[tokio::test]
async fn the_page_draws_a_bar_per_account() {
    let html = render(coverage().await);
    assert!(html.contains(&format!(r#"href="{}""#, account_href("Liabilities:Card"))), "{html}");
    let text = visible(&html);
    position(&text, "對過數單");
    // The days are written on the bar: checked through, then recorded
    // through where records run past the check.
    position(&text, "月票 無固定數單 2024-02-01 設定");
    position(&text, "現金 每30工對一擺 2024-01-31 2024-02-02 1 筆猶未對 手動對過");
    position(
        &text,
        "信用卡 甲銀行 · 逐月數單（25 號） 2023-12-25 2024-01-20 逾期 45 日 猶欠 1–2 月的數單",
    );
    // Statements check a card; it is never confirmed by hand.
    let card = &text[text.find("信用卡 甲銀行").unwrap()..];
    assert!(!card[..card.find("設定").unwrap()].contains("手動對過"), "{card}");
    // A split account opens on the bar, its one date shown once; an
    // instalment spans its ends.
    position(&text, "甲公司 無固定數單 2024-01-05 欠你 200 TWD");
    position(&text, "閱讀器 分期付款 2024-01-10 2024-11-20 已繳 2/12 期");
    // A card awaiting its statements, drawn apart from records.
    assert!(html.contains(r#"<i class="missing""#), "{html}");
    // The emptied wallet is there, hidden until asked for.
    assert!(html.contains(r#"class="account unscheduled hidden""#), "{html}");
}

/// Runs a server function as the router would, with the chart's split roots.
async fn call<F: std::future::Future>(l: &Ledger, f: impl FnOnce() -> F) -> F::Output {
    let config = server::Config { splits: splits(), ..Default::default() };
    Context::from(&l.pool).config(config).call(f).await
}

/// Without these rules the bars have no geometry, and no other test would
/// notice: a rebase once dropped the page's styles and everything still passed.
#[test]
fn the_stylesheet_draws_the_bars() {
    let css = include_str!("../style/main.css");
    for rule in [".coverage .bar i", ".coverage .account", ".count-badge.due"] {
        assert!(css.contains(rule), "main.css has no {rule}");
    }
}

#[tokio::test]
async fn the_server_functions_read_and_count() {
    let l = ledger().await;
    let c = call(&l, load_coverage).await.unwrap();
    assert_eq!(c.today, chrono::Local::now().date_naive().to_string());
    // Long past every due day of the fixture, whatever today is.
    assert_eq!(account(&c, "Liabilities:Card").status, Standing::Overdue);
    // The bank account, the card, cash and the broker.
    assert_eq!(call(&l, coverage_due).await.unwrap(), 4);
    assert_eq!(reminded(&c).count(), 4);
}

#[tokio::test]
async fn a_schedule_is_set_from_the_form() {
    let l = ledger().await;
    let save = |kind, month: Option<&str>, day: Option<&str>| {
        let (month, day) = (month.map(Into::into), day.map(Into::into));
        call(&l, move || {
            save_schedule(
                "Assets:Bank:FX".into(),
                "甲銀行".into(),
                kind,
                month,
                day,
                None,
                "20".into(),
                Some("2023-12-01".into()),
            )
        })
    };
    save(CadenceKind::Yearly, Some("12"), Some("31")).await.unwrap();
    let saved = db::coverage::schedules(&mut *l.pool.acquire().await.unwrap()).await.unwrap();
    let fx = saved.iter().find(|s| s.account == "Assets:Bank:FX").unwrap();
    assert_eq!(fx.bank.as_deref(), Some("甲銀行"));
    assert_eq!((fx.cadence, fx.grace), (Cadence::Yearly { month: 12, day: 31 }, 20));
    assert_eq!(fx.since, Some("2023-12-01".parse().unwrap()));

    let refused = save(CadenceKind::Monthly, None, None).await.unwrap_err();
    assert!(said(&refused).contains("結數日"), "{refused}");
}

async fn confirm(l: &Ledger, amount: &str, correct: bool) -> Result<(), ServerFnError> {
    let amount = amount.to_string();
    call(l, move || {
        confirm_balance(
            "Assets:Prepaid:Gift".into(),
            "2024-02-29".into(),
            "TWD".into(),
            amount,
            correct.then(|| "1".into()),
        )
    })
    .await
}

#[tokio::test]
async fn a_balance_confirmed_by_hand_checks_the_account() {
    let l = ledger().await;
    let refused = confirm(&l, "299", false).await.unwrap_err();
    assert_eq!(said(&refused), "帳簿 2024-02-29 是 300 TWD，毋是 299");
    confirm(&l, "300", false).await.unwrap();
    let c = read(&l).await;
    let gift = account(&c, "Assets:Prepaid:Gift");
    assert_eq!(gift.checked, mark("2024-02-29", 10));
    assert_eq!(gift.tail, None);
}

#[tokio::test]
async fn a_different_count_books_its_correction_for_review() {
    let l = ledger().await;
    confirm(&l, "280", true).await.unwrap();
    let c = read(&l).await;
    assert_eq!(account(&c, "Assets:Prepaid:Gift").checked, mark("2024-02-29", 10));
    // The 20 that went missing, spent to the fallback category and awaiting
    // a name in 待確認.
    let (narration, reviewed, amount): (String, bool, String) = sqlx::query_as(
        "SELECT t.narration, t.reviewed, p.amount FROM transactions t
         JOIN postings p ON p.transaction_id = t.id JOIN accounts a ON a.id = p.account_id
         WHERE a.path = 'Expenses:Uncategorized'",
    )
    .fetch_one(&l.pool)
    .await
    .unwrap();
    assert_eq!(
        (narration.as_str(), reviewed, amount.as_str()),
        ("對帳校正：實際 280 TWD", false, "20")
    );
}

async fn schedule_gift(l: &Ledger, cadence: Cadence, since: Option<&str>) {
    let schedule = db::coverage::Schedule {
        account: "Assets:Prepaid:Gift".into(),
        bank: None,
        cadence,
        grace: 10,
        since: since.map(|s| s.parse().unwrap()),
    };
    db::coverage::set_schedule(&mut *l.pool.acquire().await.unwrap(), &schedule).await.unwrap();
}

#[tokio::test]
async fn records_before_the_banks_history_are_not_unchecked() {
    let l = ledger().await;
    schedule_gift(&l, Cadence::Never, Some("2024-01-01")).await;
    let c = read(&l).await;
    let gift = account(&c, "Assets:Prepaid:Gift");
    // Its one record, 2023-11-01, predates the history: nothing to check.
    assert_eq!(gift.tail, None);
    assert_eq!(gift.since, mark("2024-01-01", 69));
}

#[tokio::test]
async fn a_loan_is_due_its_monthly_payment_record() {
    let l = ledger().await;
    schedule_gift(&l, Cadence::Payment { day: 1 }, None).await;
    let c = read(&l).await;
    let gift = account(&c, "Assets:Prepaid:Gift");
    // Its last record is 2023-11-01: no payment since 12-01, by its records
    // rather than any figure.
    assert_eq!(CadenceName(gift.cadence).to_string(), "逐月還款（1 號）");
    assert_eq!(gift.status, Standing::Overdue);
    // A payment is plain from the bar: nothing to fetch.
    assert_eq!(gift.wanted, None);
    let html = render(c);
    position(&visible(&html), "逾期 100 日");
}

#[tokio::test]
async fn the_route_renders_and_is_in_the_nav() {
    let site = Site::serving(ledger).await;
    let html = site.page("/coverage").await;
    assert!(html.contains("對帳進度 · 帳簿"), "{html}");
    assert!(html.contains(r#"href="/coverage""#), "{html}");
    let text = visible(&html);
    position(&text, "對過數單");
    position(&text, "活存");
}
