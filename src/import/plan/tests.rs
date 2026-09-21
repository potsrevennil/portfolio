use rust_decimal_macros::dec;

use super::*;
use crate::ledger::statements::cathay::StatementLine;

fn chart() -> Chart {
    toml::from_str(
        r#"
        [institution.accounts]
        "111111111111" = "Assets:Bank:Savings"
        [institution]
        clearing = "Assets:Bank:Clearing"
        [fallback]
        income = "Income:Uncategorized"
        expense = "Expenses:Uncategorized"
        "#,
    )
    .expect("valid chart")
}

fn day(d: u32) -> NaiveDate { NaiveDate::from_ymd_opt(2026, 6, d).expect("date") }

fn statement(lines: &[(u32, Decimal, Decimal)]) -> BankStatement {
    BankStatement {
        account_no: "111111111111".into(),
        account_kind: "活存".into(),
        currency: Currency::TWD,
        period_end: None,
        lines: lines
            .iter()
            .map(|&(d, delta, balance)| StatementLine {
                book_date: day(d),
                description: String::new(),
                withdrawal: -delta.min(Decimal::ZERO),
                deposit: delta.max(Decimal::ZERO),
                balance,
                info: String::new(),
                memo: String::new(),
            })
            .collect(),
    }
}

fn held(d: u32, amount: Decimal, opening: bool) -> LedgerPosting {
    LedgerPosting { date: day(d), amount, external_ref: None, opening }
}

fn run(st: &BankStatement, existing: Vec<LedgerPosting>) -> Result<Plan> {
    let s = Statement {
        account: "Assets:Bank:Savings".into(),
        statement: st,
        files: vec![0; st.lines.len()],
        existing,
    };
    plan(&chart(), &[s], &HashSet::new(), &[])
}

#[test]
fn a_new_account_gets_one_opening_before_its_first_line() -> Result<()> {
    let st = statement(&[(2, dec!(-100), dec!(400)), (3, dec!(50), dec!(450))]);
    let p = run(&st, vec![])?;
    assert_eq!(p.counts.openings, 1);
    assert_eq!(p.openings, vec![day(1)]);
    assert_eq!(p.transactions[0].1.postings[0].amount, dec!(500));
    assert_eq!(p.transactions.len(), 3);
    Ok(())
}

/// Lines up to the opening are in its amount; later ones chain from it.
#[test]
fn lines_through_the_opening_date_are_skipped() -> Result<()> {
    let st = statement(&[(1, dec!(100), dec!(100)), (3, dec!(5), dec!(105))]);
    let p = run(&st, vec![held(1, dec!(100), true)])?;
    assert_eq!(p.counts.predate_opening, 1);
    assert_eq!(p.counts.new, 1);
    assert_eq!(p.counts.openings, 0);
    Ok(())
}

#[test]
fn postings_without_an_opening_cannot_be_chained() {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let err = run(&st, vec![held(1, dec!(100), false)]).expect_err("no opening");
    assert!(err.to_string().contains("no opening"), "{err}");
}

#[test]
fn two_openings_are_refused() {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let err =
        run(&st, vec![held(1, dec!(100), true), held(2, dec!(0), true)]).expect_err("two openings");
    assert!(err.to_string().contains("more than one opening"), "{err}");
}

/// A posting the ledger holds that no statement line explains breaks the
/// chain, rather than being silently kept alongside the imported lines.
#[test]
fn a_ledger_that_disagrees_with_the_statement_is_refused() {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let err =
        run(&st, vec![held(1, dec!(100), true), held(2, dec!(-1), false)]).expect_err("off by one");
    assert!(err.to_string().contains("nothing was imported"), "{err}");
}
