use rust_decimal_macros::dec;

use super::*;

fn day(d: &str) -> NaiveDate { d.parse().unwrap() }

fn line(posted: &str, amount: Decimal) -> Line {
    Line {
        purchased: None,
        posted: day(posted),
        description: "咖啡".into(),
        amount,
        detail: String::new(),
    }
}

fn bill(closes: &str, opening: Decimal, closing: Decimal, lines: Vec<Line>) -> Bill {
    Bill { closes: day(closes), opening, closing, lines }
}

#[test]
fn lines_run_in_posting_order_with_a_liabilitys_balance() -> Result<()> {
    let s = bill("2026-01-20", dec!(100), dec!(130), vec![
        line("2026-01-10", dec!(50)),
        line("2026-01-05", dec!(-20)),
    ])
    .into_statement(Bank::CathayCard, "c")?;

    let seen: Vec<(Decimal, Decimal)> = s.lines.iter().map(|l| (l.delta(), l.balance)).collect();
    assert_eq!(seen, [(dec!(20), dec!(-80)), (dec!(-50), dec!(-130))]);
    assert_eq!(s.periods[0].opening, dec!(-100));
    assert_eq!(s.periods[0].closing, dec!(-130));
    Ok(())
}

#[test]
fn lines_that_do_not_add_up_to_the_closing_are_refused() {
    let wrong = bill("2026-01-20", dec!(100), dec!(131), vec![line("2026-01-10", dec!(30))]);
    assert!(wrong.into_statement(Bank::CathayCard, "c").is_err());
}

#[test]
fn a_line_posted_after_the_closing_is_refused() {
    let late = bill("2026-01-20", dec!(0), dec!(30), vec![line("2026-01-21", dec!(30))]);
    assert!(late.into_statement(Bank::CathayCard, "c").is_err());
}

#[test]
fn instalment_lines_are_told_by_their_part() {
    assert!(is_instalment("家電 第04/06期 /TW"));
    assert!(is_instalment("家電第1/3期"));
    assert!(!is_instalment("第一銀行"));
    assert!(!is_instalment("第二期款"));
}

#[test]
fn only_the_next_closing_on_the_same_balance_follows() {
    let (closed, end) = (day("2026-01-18"), day("2026-02-22"));
    assert!(follows(closed, dec!(5), end, dec!(5)));
    assert!(!follows(closed, dec!(5), end, dec!(6)));
    assert!(!follows(closed, dec!(5), day("2026-03-17"), dec!(5)));
    assert!(follows(day("2025-12-17"), dec!(5), day("2026-01-18"), dec!(5)));
    // A closing date that shifts across a month's end misses no statement.
    assert!(follows(day("2026-01-31"), dec!(5), day("2026-03-02"), dec!(5)));
    assert!(follows(day("2026-01-01"), dec!(5), day("2026-01-31"), dec!(5)));
}

fn figure(
    account: &str,
    start: &str,
    end: &str,
    opening: Decimal,
    closing: Decimal,
) -> BalanceAssertion {
    BalanceAssertion {
        source: AssertionSource::Statement,
        account: account.to_string(),
        currency: Currency::TWD,
        period_start: Some(day(start)),
        opening: Some(opening),
        period_end: day(end),
        closing,
    }
}

#[test]
fn a_figure_starts_late_only_after_the_statement_it_follows() {
    let recorded = [
        figure("A", "2026-01-01", "2026-01-20", dec!(0), dec!(10)),
        figure("A", "2026-01-21", "2026-02-20", dec!(10), dec!(20)),
        figure("A", "2026-03-08", "2026-03-20", dec!(20), dec!(30)),
        figure("A", "2026-05-08", "2026-05-20", dec!(30), dec!(40)),
        figure("B", "2026-06-08", "2026-06-20", dec!(40), dec!(50)),
    ];
    let late: Vec<_> = late_starts(&recorded).into_iter().map(|(a, d)| (a.period_end, d)).collect();
    assert_eq!(late, [(day("2026-03-20"), day("2026-02-21"))]);
}

fn statement(closes: &str, opening: Decimal, closing: Decimal) -> (BankStatement, PathBuf) {
    let s = bill(closes, opening, closing, vec![line(closes, closing - opening)]);
    (s.into_statement(Bank::CathayCard, "c").unwrap(), PathBuf::from(closes))
}

#[test]
fn consecutive_months_must_carry_the_balance_over() {
    let broken = merge(vec![
        statement("2026-01-20", dec!(0), dec!(10)),
        statement("2026-02-20", dec!(11), dec!(20)),
    ]);
    assert!(broken.is_err());
}

#[test]
fn a_statement_repeated_is_refused() {
    let twice = merge(vec![
        statement("2026-01-20", dec!(0), dec!(10)),
        statement("2026-01-20", dec!(0), dec!(10)),
    ]);
    assert!(twice.is_err());
}

/// Beside another statement it is a figure with nothing to add; by itself it
/// gives the ledger no line to check against.
#[test]
fn a_statement_with_no_line_needs_a_sibling() -> Result<()> {
    let empty = |closes: &str, balance| {
        let s = bill(closes, balance, balance, vec![]);
        (s.into_statement(Bank::CathayCard, "c").unwrap(), PathBuf::from(closes))
    };
    assert!(merge(vec![empty("2026-02-20", dec!(10))]).is_err());
    merge(vec![statement("2026-01-20", dec!(0), dec!(10)), empty("2026-02-20", dec!(10))])?;
    Ok(())
}

#[test]
fn a_missing_month_leaves_the_periods_apart() -> Result<()> {
    let merged = merge(vec![
        statement("2026-03-20", dec!(10), dec!(20)),
        statement("2026-01-20", dec!(0), dec!(10)),
    ])?;
    let [one] = merged.as_slice() else { panic!("one card") };
    let periods = &one.statement.periods;
    assert_eq!(periods[1].start, day("2026-03-20"));
    assert_eq!(periods[0].end, day("2026-01-20"));
    Ok(())
}
