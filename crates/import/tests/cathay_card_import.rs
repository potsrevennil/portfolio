//! Cathay card statements over records the freeze booked from 天天記帳, on
//! invented figures: what a statement's closing vouches for, and what it
//! reports.

mod common;

use anyhow::Result;
use common::card::{records, Fixture, CARD};
use db::import::{ImportStore, Posting, Transaction};
use ledger::{journal::UNVERIFIED_TAG, labels::Labels, model::Source, statements::bank::Bank};
use ledger_types::{assertion::AssertionSource, currency::Currency};
use rust_decimal_macros::dec;

const MAIN: &str = "Assets:Bank:Main";

/// One row of 帳單明細: 消費日, 交易說明, 新臺幣金額, 入帳起息日.
type Row<'a> = (&'a str, &'a str, &'a str, &'a str);

/// A statement in the CSV's layout.
fn statement(closes: &str, opening: u32, closing: u32, rows: &[Row]) -> String {
    let mut csv = format!(
        "\u{feff}\"{closes}信用卡對帳單\"\r\n\"帳單資訊\"\r\n\"最低應繳總金額\",\"\
         本期帳單未付金額\",\"本期帳單已繳金額\",\"帳單結帳日\"\r\n\"TWD \
         1\",\"−\",\"−\",\"{closes}\"\r\n\"應繳總金額計算\"\r\n\"上期帳單總額\",\"繳款金額\",\"\
         本期帳單/退款/調整\",\"本期/分期預借金額\",\"循環利息\",\"餘額代償利息\",\"本期應繳總額\"\
         \r\n\"TWD {opening}\",\"TWD 0\",\"TWD 0\",\"TWD 0\",\"TWD 0\",\"TWD 0\",\"TWD \
         {closing}\"\r\n\"帳單明細\"\r\n\"新臺幣\"\r\n\"消費日\",\"交易說明\",\"新臺幣金額\",\"\
         卡號/行動末四碼\",\"消費國家/幣別\",\"消費金額\",\"入帳起息日\",\"折算日\"\r\n\"−\",\"\
         上期帳單總額\",\"{opening}\",\"−\",\"−\",\"−\",\"−\",\"−\"\r\n"
    );
    for (purchased, description, amount, posted) in rows {
        csv.push_str(&format!(
            "\"{purchased}\",\"{description}\",\"{amount}\",\"0000\",\"TW / \
             TWD\",\"−\",\"{posted}\",\"−\"\r\n"
        ));
    }
    csv.push_str("\r\n\"信用卡回饋\"\r\n\"上期累積點數\",\"本期新增/扣除點數\"\r\n\"1\",\"2\"\r\n");
    csv
}

fn leg(date: &str, posted: Option<&str>, amount: &str) -> (String, Option<String>, String) {
    (date.to_string(), posted.map(String::from), amount.to_string())
}

/// A card payment is one record with a leg on each statement's account.
/// Whichever statement comes first, the record keeps the date written and each
/// leg counts on its own statement's day.
#[tokio::test]
async fn the_order_of_two_banks_imports_changes_no_date() -> Result<()> {
    let mut seen = Vec::new();
    for bank_first in [true, false] {
        let f = Fixture::frozen(&records(&[("2026-01-02", "expense", 100, "飲食")])).await?;
        let tagged = |account: &str, amount| Posting {
            tags: Some(UNVERIFIED_TAG.to_string()),
            ..(account, amount, Currency::TWD).into()
        };
        ImportStore::new(f.pool.clone(), Labels::from(&f.chart))
            .insert_deduped(&Transaction {
                date: "2026-01-05".parse()?,
                payee: None,
                narration: None,
                source: Source::Tiantian,
                external_ref: Some("tiantian:T1".into()),
                import_batch_id: None,
                postings: vec![tagged(MAIN, dec!(-100)), tagged(CARD, dec!(100))],
            })
            .await?;
        let bank = f.write(
            "savings-2026.csv",
            "111111111111 \
             活存\n幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n2025/12/01,\
             2025/12/01,存入,,1000,1000,,\n2026/01/06,2026/01/06,繳卡費,100,,900,,\n",
        )?;
        let card = f.write(
            "2026-01.csv",
            &statement("2026/01/20", 0, 0, &[
                ("01/02", "咖啡", "100", "01/03"),
                ("01/05", "繳款", "-100", "01/07"),
            ]),
        )?;
        let (first, second) = match bank_first {
            true => ((Bank::Cathay, &bank), (Bank::CathayCard, &card)),
            false => ((Bank::CathayCard, &card), (Bank::Cathay, &bank)),
        };
        f.import(first.0, std::slice::from_ref(first.1)).await?;
        f.import(second.0, std::slice::from_ref(second.1)).await?;
        let legs: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT t.date, a.path, p.posted_date FROM postings p
             JOIN transactions t ON t.id = p.transaction_id
             JOIN accounts a ON a.id = p.account_id
             WHERE t.external_ref = 'tiantian:T1' ORDER BY a.path",
        )
        .fetch_all(&f.pool)
        .await?;
        seen.push(legs);
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[0], [
        ("2026-01-05".into(), MAIN.into(), Some("2026-01-06".into())),
        ("2026-01-05".into(), CARD.into(), Some("2026-01-07".into())),
    ]);
    Ok(())
}

/// A purchase stays on its date and moves the balance when it posts, so a
/// closing vouches for what has posted, and the app's own closing retires.
#[tokio::test]
async fn a_closing_vouches_for_what_has_posted_and_records_keep_their_dates() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2026-01-05", "expense", 100, "飲食"),
        ("2026-01-08", "expense", 50, "飲食"),
    ]))
    .await?;
    let january = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 150, &[
            ("01/05", "咖啡", "100", "01/08"),
            ("01/08", "便當", "50", "01/10"),
        ]),
    )?;

    let report = f.import(Bank::CathayCard, &[january]).await?;

    assert_eq!((report.inserted, report.counts.verified), (0, 2));
    assert_eq!(f.legs().await?, vec![
        leg("2026-01-05", Some("2026-01-08"), "-100"),
        leg("2026-01-08", Some("2026-01-10"), "-50"),
    ]);
    let figures = f.figures().await?;
    let [figure] = figures.as_slice() else { panic!("{figures:?}") };
    assert_eq!(figure.source, AssertionSource::Statement);
    assert_eq!(figure.closing, dec!(-150));
    assert!(f.gate_passes().await?);
    Ok(())
}

#[tokio::test]
async fn importing_a_statement_again_adds_nothing() -> Result<()> {
    let f = Fixture::frozen(&records(&[("2026-01-05", "expense", 100, "飲食")])).await?;
    let path = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 130, &[
            ("01/05", "咖啡", "100", "01/08"),
            ("01/06", "無記錄", "30", "01/09"),
        ]),
    )?;

    f.import(Bank::CathayCard, std::slice::from_ref(&path)).await?;
    let again = f.import(Bank::CathayCard, &[path]).await?;

    assert_eq!((again.inserted, again.counts.known), (0, 2));
    assert_eq!(f.legs().await?.len(), 2);
    Ok(())
}

/// A line with no record is a real line: booked on its purchase date under its
/// own namespaced ref, for the review queue to name.
#[tokio::test]
async fn a_line_with_no_record_is_booked_uncategorised() -> Result<()> {
    let f = Fixture::frozen(&records(&[("2026-01-05", "expense", 100, "飲食")])).await?;
    let path = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 130, &[
            ("01/05", "咖啡", "100", "01/08"),
            ("01/06", "無記錄", "30", "01/09"),
        ]),
    )?;

    let report = f.import(Bank::CathayCard, &[path]).await?;

    assert_eq!((report.inserted, report.counts.uncategorised), (1, 1));
    assert!(f.legs().await?.contains(&leg("2026-01-06", Some("2026-01-09"), "-30")));
    let reference: String =
        sqlx::query_scalar("SELECT external_ref FROM transactions WHERE date = '2026-01-06'")
            .fetch_one(&f.pool)
            .await?;
    assert!(reference.starts_with("cathay-card:testcard:2026-01-09:"), "{reference}");
    Ok(())
}

/// A purchase on the statement's last days is on the next one: it waits,
/// unverified and outside this closing, and the next statement stamps it.
#[tokio::test]
async fn a_purchase_still_to_post_waits_for_the_next_statement() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2026-01-05", "expense", 100, "飲食"),
        ("2026-01-19", "expense", 40, "飲食"),
    ]))
    .await?;
    let january = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 100, &[("01/05", "咖啡", "100", "01/08")]),
    )?;
    let february = f.write(
        "2026-02.csv",
        &statement("2026/02/20", 100, 140, &[("01/19", "便當", "40", "01/22")]),
    )?;

    f.import(Bank::CathayCard, &[january]).await?;
    assert_eq!(f.unverified().await?, 1);
    assert!(f.gate_passes().await?);
    assert!(f.legs().await?.contains(&leg("2026-01-19", Some("2026-01-21"), "-40")));

    f.import(Bank::CathayCard, &[february]).await?;

    assert_eq!(f.unverified().await?, 0);
    assert!(f.legs().await?.contains(&leg("2026-01-19", Some("2026-01-22"), "-40")));
    assert!(f.gate_passes().await?);
    Ok(())
}

/// A purchase on a statement's last days that the next one in hand doesn't show
/// may have posted on the one missing between them: it waits past the first,
/// unverified, whichever is imported.
#[tokio::test]
async fn a_purchase_before_a_missing_month_waits_past_its_statement() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2026-01-05", "expense", 100, "飲食"),
        ("2026-01-19", "expense", 40, "飲食"),
        ("2026-03-05", "expense", 30, "飲食"),
    ]))
    .await?;
    let january = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 100, &[("01/05", "咖啡", "100", "01/08")]),
    )?;
    let march = f.write(
        "2026-03.csv",
        &statement("2026/03/20", 140, 170, &[("03/05", "便當", "30", "03/08")]),
    )?;

    f.import(Bank::CathayCard, &[january, march]).await?;

    assert_eq!(f.unverified().await?, 1);
    assert!(f.legs().await?.contains(&leg("2026-01-19", Some("2026-01-21"), "-40")));
    assert!(f.gate_passes().await?);
    Ok(())
}

/// A record from before the statement leaves its opening off: the import goes
/// on, books what it can, and reports the gap.
#[tokio::test]
async fn an_old_record_no_line_shows_is_reported_not_refused() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2025-10-01", "expense", 70, "飲食"),
        ("2026-01-05", "expense", 100, "飲食"),
    ]))
    .await?;
    let path = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 100, &[("01/05", "咖啡", "100", "01/08")]),
    )?;

    let report = f.import(Bank::CathayCard, &[path]).await?;

    assert_eq!(report.gaps.len(), 1, "{report}");
    let posted: Vec<_> = f.legs().await?.into_iter().map(|(_, posted, _)| posted).collect();
    assert_eq!(posted, [None, Some("2026-01-08".to_string())]);
    assert!(f.figures().await?.iter().any(|a| a.source == AssertionSource::Statement));
    assert!(!f.gate_passes().await?);
    Ok(())
}

/// A statement that covers a record's day and passed it by says so on the
/// record, which stays where it is.
#[tokio::test]
async fn a_record_its_statements_passed_by_is_suggested_for_a_look() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2026-01-06", "expense", 100, "飲食"),
        ("2026-01-10", "expense", 70, "飲食"),
        ("2026-02-05", "expense", 50, "飲食"),
    ]))
    .await?;
    let january = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 100, &[("01/06", "咖啡", "100", "01/08")]),
    )?;
    let february = f.write(
        "2026-02.csv",
        &statement("2026/02/20", 100, 150, &[("02/05", "咖啡", "50", "02/08")]),
    )?;

    let report = f.import(Bank::CathayCard, &[january, february]).await?;

    let said = report.run.to_string();
    assert_eq!(said.matches("not on the statement through").count(), 1, "{said}");
    assert!(f
        .legs()
        .await?
        .iter()
        .any(|(date, posted, _)| date == "2026-01-10" && posted.is_none()));
    Ok(())
}

/// A purchase and its foreign fee post apart, and the record is of both: its
/// leg divides so the balance moves on each day, keeping the record's category.
#[tokio::test]
async fn lines_one_record_covers_split_its_leg() -> Result<()> {
    let f = Fixture::frozen(&records(&[("2026-01-05", "expense", 103, "飲食")])).await?;
    let path = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 103, &[
            ("01/05", "ABROAD", "100", "01/06"),
            ("01/05", "國外交易手續費", "3", "01/15"),
        ]),
    )?;

    let report = f.import(Bank::CathayCard, &[path]).await?;

    assert_eq!((report.inserted, report.counts.verified), (0, 2));
    assert_eq!(f.legs().await?, vec![
        leg("2026-01-05", Some("2026-01-06"), "-100"),
        leg("2026-01-05", Some("2026-01-15"), "-3"),
    ]);
    let event: String =
        sqlx::query_scalar("SELECT kind FROM transaction_events").fetch_one(&f.pool).await?;
    assert_eq!(event, "split");
    assert!(f.gate_passes().await?);
    Ok(())
}

/// A 第 n/m 期 line is a part billed on its posting date, whatever purchase it
/// prints; interest prints no date at all and is charged at the closing.
#[tokio::test]
async fn instalment_parts_and_interest_do_not_break_the_check() -> Result<()> {
    let f = Fixture::frozen(&records(&[("2026-01-05", "expense", 100, "飲食")])).await?;
    let path = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 345, &[
            ("01/05", "咖啡", "100", "01/08"),
            ("10/16", "家電 第02/12期", "200", "01/12"),
            ("−", "循環利息", "45", "−"),
        ]),
    )?;

    f.import(Bank::CathayCard, &[path]).await?;

    let legs = f.legs().await?;
    assert!(legs.contains(&leg("2026-01-12", Some("2026-01-12"), "-200")), "{legs:?}");
    assert!(legs.contains(&leg("2026-01-20", Some("2026-01-20"), "-45")), "{legs:?}");
    assert!(f.gate_passes().await?);
    Ok(())
}

/// A statement states no period start: it runs from the one it follows, in
/// this run or an earlier one. A missing month stays a gap until it comes.
#[tokio::test]
async fn a_statement_starts_after_the_one_it_follows() -> Result<()> {
    let f = Fixture::frozen(&records(&[
        ("2026-01-05", "expense", 100, "飲食"),
        ("2026-02-05", "expense", 20, "飲食"),
        ("2026-04-05", "expense", 30, "飲食"),
    ]))
    .await?;
    let january = f.write(
        "2026-01.csv",
        &statement("2026/01/20", 0, 100, &[("01/05", "咖啡", "100", "01/08")]),
    )?;
    let february = f.write(
        "2026-02.csv",
        &statement("2026/02/20", 100, 120, &[("02/05", "便當", "20", "02/08")]),
    )?;
    let april = f.write(
        "2026-04.csv",
        &statement("2026/04/20", 120, 150, &[("04/05", "便當", "30", "04/08")]),
    )?;

    f.import(Bank::CathayCard, &[january]).await?;
    f.import(Bank::CathayCard, &[february]).await?;
    f.import(Bank::CathayCard, &[april]).await?;

    let figures = f.figures().await?;
    let starts: Vec<String> =
        figures.iter().map(|a| a.period_start.expect("states a period").to_string()).collect();
    assert_eq!(starts, ["2026-01-08", "2026-01-21", "2026-04-08"]);
    assert!(f.gate_passes().await?);

    let march = f.write(
        "2026-03.csv",
        &statement("2026/03/20", 120, 120, &[
            ("03/08", "便當", "10", "03/08"),
            ("03/09", "退款", "-10", "03/09"),
        ]),
    )?;
    f.import(Bank::CathayCard, &[march]).await?;

    let figures = f.figures().await?;
    let starts: Vec<String> =
        figures.iter().map(|a| a.period_start.expect("states a period").to_string()).collect();
    assert_eq!(starts, ["2026-01-08", "2026-01-21", "2026-02-21", "2026-03-21"]);
    assert!(f.gate_passes().await?);
    Ok(())
}
