use chrono::Datelike;
use rust_decimal_macros::dec;

use super::*;

/// `pdftotext -raw` of an invented statement, in the real layout: the sample
/// 2019 rows sit among the real ones, a line of control characters ends each
/// page's rows, a 備註 wraps, a row prints no balance, a friend's transfer is
/// called back, and the USD account is idle.
const MARCH: &str = "\
LINE Bank 連線商業銀行 2026年03月對帳單
對帳單期間:20260301-20260331
$0
資產總額
● 主帳戶 $1,150 ● 外幣 $0
\u{1}\u{2} \u{3}\u{4}
2019/10/09 ATM -40,000 $199,960,000 0130017T
2019/10/11 Remittance +8,000 $199,968,000 Salary
 $837,992
1 / 3台幣存款總餘額
$1,150
台幣存款交易明細
主帳戶
主帳戶
$1,150 *******00042
主帳戶
主帳戶 *******00042 $1,150
日期 交易說明 交易金額 餘額 備註
2026.03.02 轉帳 $1,000 $1,100
範例商業銀行 ***********
00077
\u{1}\u{2} \u{3}\u{4}
2019/10/02 ACH -100,000,000 $200,000,000 CTBC Bank
 $7,336,870
2 / 3日期 交易說明 交易金額 餘額 備註
2026.03.05 LINE好友轉帳 -$300 $800 阿明
2026.03.05 取消轉帳 $300 $1,100 取消.阿明
2026.03.05 LINE好友轉帳 -$300 $800 阿明
2026.03.09 存款利息 $50 利息
2026.03.10 轉帳 $300 $1,150 範例4321
\u{1}\u{2} \u{3}\u{4}
2019/10/13 ATM -40,000 $199,960,000 0130017T
 $837,992
範例頁尾 02-0000-0000
3 / 3外幣存款總餘額
$0
等值台幣
外幣存款交易明細
外幣主帳戶
*******00099
美元 0.00 USD
外幣主帳戶
美元 *******00099 0.00 USD
本月無交易紀錄
簽帳金融卡消費總金額
簽帳金融卡交易明細
2026.03.20 消費 -$10 $1,140 範例商店
";

fn march() -> Document { MARCH.parse().expect("parses") }

#[test]
fn reads_the_rows_and_drops_the_sample_ones() {
    let doc = march();
    assert_eq!((doc.start, doc.end), (day(3, 1), day(3, 31)));
    assert_eq!(doc.account_mask, "*******00042");
    let twd = &doc.sections[0];
    assert_eq!((twd.currency, twd.opening, twd.closing), (Currency::TWD, dec!(100), dec!(1150)));
    let deltas: Vec<Decimal> = twd.lines.iter().map(StatementLine::delta).collect();
    assert_eq!(deltas, [dec!(1000), dec!(-300), dec!(300), dec!(-300), dec!(50), dec!(300)]);
    assert!(twd.lines.iter().all(|l| l.book_date.year() == 2026));
}

#[test]
fn a_wrapped_remark_is_joined() {
    assert_eq!(march().sections[0].lines[0].info, "範例商業銀行 ***********00077");
}

/// The page's footer follows its last row; it is not part of the 備註.
#[test]
fn the_last_row_on_a_page_keeps_only_its_own_remark() {
    assert_eq!(march().sections[0].lines[5].info, "範例4321");
}

/// Only some rows print a balance; the others are chained from the one before.
#[test]
fn a_row_without_a_balance_gets_one_from_the_rows_before() {
    let lines = &march().sections[0].lines;
    assert_eq!((lines[4].balance, lines[4].info.as_str()), (dec!(850), "利息"));
    assert_eq!(lines[5].balance, dec!(1150));
}

#[test]
fn a_first_row_without_a_balance_chains_back_from_the_closing() {
    let text = MARCH.replace("2026.03.02 轉帳 $1,000 $1,100", "2026.03.02 轉帳 $1,000");
    let twd = &text.parse::<Document>().expect("parses").sections[0];
    assert_eq!((twd.opening, twd.lines[0].balance), (dec!(100), dec!(1100)));
}

#[test]
fn an_idle_foreign_account_has_a_closing_and_no_rows() {
    let doc = march();
    assert_eq!(doc.sections.len(), 2);
    let usd = &doc.sections[1];
    assert_eq!((usd.currency, usd.opening, usd.closing), (Currency::USD, dec!(0), dec!(0)));
    assert!(usd.lines.is_empty());
}

#[test]
fn a_printed_balance_the_rows_disagree_with_is_an_error() {
    let text = MARCH.replace("$300 $1,150 範例4321", "$300 $1,151 範例4321");
    let err = text.parse::<Document>().expect_err("1,151 does not follow");
    assert!(format!("{err:#}").contains("prints a balance of 1151"), "{err:#}");
}

#[test]
fn rows_that_do_not_reach_the_closing_are_an_error() {
    let text = MARCH
        .replace("主帳戶 *******00042 $1,150", "主帳戶 *******00042 $1,200")
        .replace("台幣存款總餘額\n$1,150", "台幣存款總餘額\n$1,200");
    let err = text.parse::<Document>().expect_err("the rows close at 1,150");
    assert!(format!("{err:#}").contains("closes at 1200"), "{err:#}");
}

#[test]
fn a_month_without_rows_opens_on_its_closing() {
    let start = MARCH.find("日期 交易說明").expect("header");
    let end = MARCH.find("3 / 3外幣").expect("foreign part");
    let text = format!("{}{}", &MARCH[..start], &MARCH[end..]);
    let doc: Document = text.parse().expect("parses");
    let twd = &doc.sections[0];
    assert!(twd.lines.is_empty());
    assert_eq!((twd.opening, twd.closing), (dec!(1150), dec!(1150)));
}

#[test]
fn foreign_currency_rows_are_refused_rather_than_guessed() {
    let text = MARCH.replace("本月無交易紀錄\n簽帳", "2026.03.03 存入 $5 $5\n簽帳");
    assert!(text.parse::<Document>().is_err());
}

#[test]
fn a_sub_account_is_refused_rather_than_dropped() {
    let text = MARCH.replace("台幣存款總餘額\n$1,150", "台幣存款總餘額\n$1,900");
    let err = text.parse::<Document>().expect_err("the total includes a sub-account");
    assert!(format!("{err:#}").contains("sub-accounts"), "{err:#}");
}

fn statement(start: NaiveDate, end: NaiveDate, section: Section) -> BankStatement {
    BankStatement {
        bank: Bank::LineBank,
        account_no: "100000000042".to_string(),
        account_kind: "活存".to_string(),
        currency: section.currency,
        period_end: Some(end),
        periods: vec![Period { start, end, opening: section.opening, closing: section.closing }],
        lines: section.lines,
    }
}

fn day(month: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, month, d).expect("valid date")
}

/// Both lines of a called-back transfer are kept, paired so neither is
/// matched; the second attempt, not called back, is a real movement.
#[test]
fn a_cancelled_transfer_pairs_with_the_attempt_it_undoes() {
    let doc = march();
    let twd = doc.sections.into_iter().next().expect("TWD");
    let s = statement(doc.start, doc.end, twd);
    assert_eq!(s.lines.len(), 6);
    assert_eq!(s.reversals(), vec![(1, 2)]);
}

/// The cancel's 備註 is longer by 取消., so it can wrap where the transfer's
/// did not; the wrap is joined without the space it broke at.
#[test]
fn a_cancel_whose_remark_wrapped_still_pairs() {
    let text = MARCH
        .replace(
            "-$300 $800 阿明\n2026.03.05 取消轉帳",
            "-$300 $800 範例 商店\n2026.03.05 取消轉帳",
        )
        .replace("$300 $1,100 取消.阿明", "$300 $1,100 取消.範例\n商店");
    let doc: Document = text.parse().expect("parses");
    let twd = doc.sections.into_iter().next().expect("TWD");
    assert_eq!(
        (twd.lines[1].info.as_str(), twd.lines[2].info.as_str()),
        ("範例 商店", "取消.範例商店")
    );
    assert_eq!(statement(doc.start, doc.end, twd).reversals(), vec![(1, 2)]);
}

/// Lines before the records begin are cut, and the month they fall in then
/// starts where the kept lines do, on the balance the cut ones left.
#[test]
fn cutting_early_lines_moves_the_month_start_with_them() {
    let doc = march();
    let twd = doc.sections.into_iter().next().expect("TWD");
    let mut s = statement(doc.start, doc.end, twd);
    assert_eq!(s.trim_before(day(3, 5)), 1);
    assert_eq!(s.periods[0].start, day(3, 5));
    assert_eq!((s.opening_balance(), s.periods[0].opening), (dec!(1100), dec!(1100)));
    let a = &s.assertions("A", s.opening_date())[0];
    assert_eq!(
        (a.period_start, a.opening, a.closing),
        (Some(day(3, 5)), Some(dec!(1100)), dec!(1150))
    );

    // Past every line of a month, it moves nothing after the cut.
    let mut feb = merge(vec![month(2, dec!(0), dec!(5)), month(3, dec!(5), dec!(5))])
        .expect("merges")
        .remove(0)
        .statement;
    feb.trim_before(day(2, 10));
    assert_eq!((feb.periods[0].start, feb.periods[0].opening), (day(2, 10), dec!(5)));
    feb.trim_before(day(3, 1));
    assert_eq!(feb.periods.len(), 1);
}

#[test]
fn a_remark_names_an_account_by_its_tail() {
    assert!(remark_names_account("範例商業銀行 ***********54321", "123400054321"));
    assert!(remark_names_account("範例銀行4321", "123400054321"));
    assert!(remark_names_account("(824)0000123400054321", "123400054321"));
    assert!(!remark_names_account("範例商業銀行 ***********98765", "123400054321"));
    // Three digits are too few to name anything.
    assert!(!remark_names_account("範例321", "123400054321"));
    assert!(!remark_names_account("PC阿(阿*明)", "123400054321"));
}

#[test]
fn the_masked_number_must_fit_the_folder() {
    assert!(mask_fits("*******00042", "100000000042"));
    assert!(!mask_fits("*******00043", "100000000042"));
    assert!(!mask_fits("******00042", "100000000042"));
}

/// A statement per month: what one covers here says how merging treats it.
fn month(m: u32, opening: Decimal, closing: Decimal) -> (BankStatement, PathBuf) {
    let end = day(m + 1, 1).pred_opt().expect("month end");
    let section = Section { currency: Currency::TWD, opening, closing, lines: Vec::new() };
    (statement(day(m, 1), end, section), PathBuf::from(format!("2026-{m:02}.pdf")))
}

#[test]
fn months_merge_into_one_statement_that_keeps_each_period() {
    let merged = merge(vec![month(3, dec!(5), dec!(5)), month(2, dec!(0), dec!(5))])
        .expect("consecutive months merge");
    let s = &merged[0].statement;
    assert_eq!(s.periods.len(), 2);
    assert_eq!(s.period_end, Some(day(3, 31)));
    assert_eq!(merged[0].paths[0], PathBuf::from("2026-02.pdf"));
    assert_eq!(s.settled_through(), day(3, 31));
    assert_eq!(s.opening_date(), day(1, 31));

    let asserted = s.assertions("Assets:Line", day(1, 31));
    assert_eq!(asserted.len(), 2);
    assert_eq!((asserted[0].period_start, asserted[0].opening), (Some(day(2, 1)), Some(dec!(0))));
    assert_eq!((asserted[1].period_end, asserted[1].closing), (day(3, 31), dec!(5)));
    // A period the account's opening falls inside vouches only for its closing.
    let late = s.assertions("Assets:Line", day(2, 10));
    assert_eq!(late.len(), 2);
    assert_eq!(late[0].period_start, None);
}

#[test]
fn a_missing_month_is_an_error() {
    let err = merge(vec![month(2, dec!(0), dec!(5)), month(4, dec!(5), dec!(5))])
        .err()
        .expect("March is missing");
    assert!(err.to_string().contains("missing"), "{err}");
}

#[test]
fn a_month_that_does_not_open_on_the_last_closing_is_an_error() {
    let err = merge(vec![month(2, dec!(0), dec!(5)), month(3, dec!(6), dec!(6))])
        .err()
        .expect("5 then 6");
    assert!(err.to_string().contains("closed at 5"), "{err}");
}
