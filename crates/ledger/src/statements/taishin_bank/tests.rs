use rust_decimal_macros::dec;

use super::*;

fn day(d: &str) -> NaiveDate { d.parse().unwrap() }

const ACCOUNT: &str = "222222222222";

fn pdf_text(rows: &str) -> String {
    format!(
        "查詢結果 (幣別 / 新臺幣)資料日期：2026/09/17 \
         17:38:52\n查詢帳號：2222-22-222222\n查詢期間：2025/01/01~2025/09/01\n交易日 帳務日 摘要 \
         支出金額 存入金額 餘額 備註\n{rows}"
    )
}

fn row(book: &str, delta: Decimal, balance: Decimal) -> Row {
    Row { book: day(book), description: "摘要".into(), delta, balance, remark: String::new() }
}

fn query(rows: Vec<Row>) -> Query {
    Query { from: day("2025-01-01"), to: day("2025-01-31"), made: day("2025-03-01"), rows }
}

#[test]
fn a_pdf_row_is_read_from_its_lines() -> Result<()> {
    let text = pdf_text("2025/01/22\n16:40:34\n2025/01/23 轉帳 8 0 92 範例 備註 08006 FIS1\n");
    let q: Query = text.parse()?;
    assert_eq!((q.from, q.to, q.made), (day("2025-01-01"), day("2025-09-01"), day("2026-09-17")));
    assert_eq!(q.rows, [Row {
        book: day("2025-01-23"),
        description: "轉帳".into(),
        delta: dec!(-8),
        balance: dec!(92),
        remark: "範例 備註 08006 FIS1".into(),
    }]);
    Ok(())
}

#[test]
fn a_wrapped_summary_and_a_wrapped_remark_stay_in_their_row() -> Result<()> {
    let rows = "2025/03/07\n19:45:23\n2025/03/07\n跨轉手續\n費\n5 0 \
                95\n2025/03/08\n10:00:00\n2025/03/08 CD轉入 0 5 100\nV 123\n456 \
                789\u{c}2025/03/09\n10:00:00\n2025/03/09 CD轉出 100 0 0\n";
    let q: Query = pdf_text(rows).parse()?;
    let seen: Vec<(&str, Decimal)> =
        q.rows.iter().map(|r| (r.description.as_str(), r.delta)).collect();
    assert_eq!(seen, [("跨轉手續費", dec!(-5)), ("CD轉入", dec!(5)), ("CD轉出", dec!(-100))]);
    assert_eq!(q.rows[1].remark, "V 123 456 789");
    Ok(())
}

#[test]
fn a_row_with_no_amounts_is_refused() {
    assert!(pdf_text("2025/03/07\n19:45:23\n2025/03/07 只有摘要\n").parse::<Query>().is_err());
}

#[test]
fn the_account_is_printed_with_dashes() {
    assert_eq!(Query::account_in(&pdf_text("")), Some("222222222222".to_string()));
}

#[test]
fn the_first_row_gives_the_opening_and_the_rest_must_follow() -> Result<()> {
    let s =
        query(vec![row("2025-01-05", dec!(-10), dec!(90)), row("2025-01-06", dec!(5), dec!(95))])
            .into_statement(ACCOUNT, "活存")?;
    assert_eq!(s.periods, [Period {
        start: day("2025-01-01"),
        end: day("2025-01-31"),
        opening: dec!(100),
        closing: dec!(95),
    }]);
    assert_eq!((s.lines[0].withdrawal, s.lines[1].deposit), (dec!(10), dec!(5)));

    let misread =
        query(vec![row("2025-01-05", dec!(-10), dec!(90)), row("2025-01-06", dec!(5), dec!(96))]);
    assert!(misread.into_statement(ACCOUNT, "活存").is_err());
    Ok(())
}

#[test]
fn a_query_with_no_rows_has_no_opening_to_read() {
    assert!(query(vec![]).into_statement(ACCOUNT, "活存").is_err());
}

/// A query made on the last day of its range may yet gain lines that day.
#[test]
fn the_day_it_was_made_is_left_to_the_next_query() -> Result<()> {
    let q = Query {
        made: day("2025-01-31"),
        ..query(vec![row("2025-01-30", dec!(-10), dec!(90)), row("2025-01-31", dec!(-5), dec!(85))])
    };
    let s = q.into_statement(ACCOUNT, "活存")?;
    assert_eq!(s.periods[0].end, day("2025-01-30"));
    assert_eq!(s.periods[0].closing, dec!(90));
    assert_eq!(s.lines.len(), 1);
    Ok(())
}

/// Queries go by 交易日: one on a range's last evening books in the next.
#[test]
fn a_row_booked_past_the_range_extends_its_period() -> Result<()> {
    let s = query(vec![row("2025-02-03", dec!(-10), dec!(90))]).into_statement(ACCOUNT, "活存")?;
    assert_eq!(s.periods[0].end, day("2025-02-03"));
    Ok(())
}

fn text_cell(s: &str) -> Data { Data::String(s.into()) }

fn sheet_row(book: &str, delta: f64, balance: f64) -> Vec<Data> {
    vec![
        text_cell("2026/01/01 10:00:00"),
        text_cell(book),
        text_cell("CD轉入"),
        Data::Float(delta),
        Data::Float(balance),
        text_cell("ATM"),
    ]
}

fn sheet(rows: Vec<Vec<Data>>, footer: &str) -> Vec<Vec<Data>> {
    let mut sheet = vec![vec![text_cell("台新銀行 - 交易明細")], vec![
        text_cell("交易日"),
        text_cell("帳務日"),
        text_cell("摘要"),
        text_cell("金額"),
        text_cell("餘額"),
        text_cell("備註"),
    ]];
    sheet.extend(rows);
    sheet.push(vec![text_cell(footer)]);
    sheet
}

fn read_sheet(rows: &[Vec<Data>]) -> Result<(NaiveDate, Vec<Row>)> {
    let rows: Vec<&[Data]> = rows.iter().map(Vec::as_slice).collect();
    sheet_rows(&rows)
}

#[test]
fn a_sheet_runs_newest_first_and_counts_its_rows() -> Result<()> {
    let rows = vec![sheet_row("2026/01/03", -5.0, 95.0), sheet_row("2026/01/02", 100.0, 100.0)];
    let (made, parsed) =
        read_sheet(&sheet(rows.clone(), "共2筆資料 資料日期：2026/09/17 17:39:15"))?;
    assert_eq!(made, day("2026-09-17"));
    let days: Vec<NaiveDate> = parsed.iter().map(|r| r.book).collect();
    assert_eq!(days, [day("2026-01-02"), day("2026-01-03")]);
    assert_eq!(parsed[1].delta, dec!(-5));

    assert!(read_sheet(&sheet(rows, "共3筆資料 資料日期：2026/09/17 17:39:15")).is_err());
    Ok(())
}

#[test]
fn a_sheets_period_is_in_its_name() -> Result<()> {
    assert_eq!(period_in_name("20250918-20260917")?, (day("2025-09-18"), day("2026-09-17")));
    assert!(period_in_name("2025").is_err());
    Ok(())
}

fn statement(
    start: &str,
    end: &str,
    opening: Decimal,
    closing: Decimal,
) -> (BankStatement, PathBuf) {
    let rows = vec![Row {
        book: day(start),
        description: "摘要".into(),
        delta: closing - opening,
        balance: closing,
        remark: String::new(),
    }];
    let q = Query { from: day(start), to: day(end), made: day("2027-01-01"), rows };
    (q.into_statement(ACCOUNT, "活存").unwrap(), PathBuf::from(end))
}

#[test]
fn queries_that_meet_carry_the_balance_and_a_gap_may_not() -> Result<()> {
    let first = || statement("2025-01-01", "2025-01-31", dec!(0), dec!(10));

    merged(vec![first(), statement("2025-02-01", "2025-02-28", dec!(10), dec!(20))])?;
    assert!(
        merged(vec![first(), statement("2025-02-01", "2025-02-28", dec!(11), dec!(20))]).is_err()
    );
    assert!(
        merged(vec![first(), statement("2025-01-20", "2025-02-28", dec!(10), dec!(20))]).is_err()
    );

    let apart = merged(vec![first(), statement("2025-03-01", "2025-03-31", dec!(99), dec!(20))])?;
    let [one] = apart.as_slice() else { panic!("one account") };
    assert_eq!(one.statement.periods.len(), 2);
    Ok(())
}
