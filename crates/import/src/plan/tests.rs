use ledger::statements::bank::StatementLine;
use rust_decimal_macros::dec;

use super::*;

fn chart() -> Chart {
    toml::from_str(
        r#"
        [institution.accounts]
        "111111111111" = "Assets:Bank:Savings"
        "222222222222" = "Assets:Bank:Other"
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
        bank: Bank::Cathay,
        account_no: "111111111111".into(),
        account_kind: "活存".into(),
        currency: Currency::TWD,
        period_end: None,
        periods: Vec::new(),
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
    LedgerPosting {
        date: day(d),
        amount,
        refs: Vec::new(),
        other_accounts: Vec::new(),
        opening,
        transaction_id: i64::from(d),
        unverified: false,
    }
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

/// With no opening, records carry the account from its first posting; they
/// must reach the statement's balance.
#[test]
fn records_without_an_opening_chain_from_their_first_posting() -> Result<()> {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let p = run(&st, vec![held(1, dec!(100), false)])?;
    let before_first_posting = day(1).pred_opt().expect("date");
    assert_eq!((p.openings, p.counts.new, p.counts.openings), (vec![before_first_posting], 1, 0));

    let err = run(&st, vec![held(1, dec!(90), false)]).expect_err("records reach 90, not 100");
    assert!(err.to_string().contains("nothing was imported"), "{err}");
    Ok(())
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

/// The account of the tests' second statement, which most of them leave out
/// of the import.
const OTHER: &str = "Assets:Bank:Other";

fn unverified(d: u32, amount: Decimal, id: i64) -> LedgerPosting {
    LedgerPosting { unverified: true, transaction_id: id, ..held(d, amount, false) }
}

fn with(st: &BankStatement, existing: Vec<LedgerPosting>) -> Result<Plan> {
    let s = Statement {
        account: "Assets:Bank:Savings".into(),
        statement: st,
        files: vec![0; st.lines.len()],
        existing,
    };
    plan(&chart(), &[s], &HashSet::new(), &[])
}

/// A line verifies the one unverified record of its amount near it: the
/// record keeps its row and takes the line's date and ref; nothing is
/// inserted for the line.
#[test]
fn a_line_verifies_its_one_unverified_record_in_place() -> Result<()> {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let p = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-20), 42)])?;
    assert_eq!(p.verified, vec![Verification {
        transaction_id: 42,
        date: Some(day(10)),
        statement_ref: st.dedup_refs()[0].clone(),
        unchecked: Vec::new(),
    }]);
    assert_eq!((p.counts.verified, p.transactions.len()), (1, 0));
    Ok(())
}

/// Two lunches of one price, two records of it: nothing picks which is
/// which, and the import stops with both records unverified.
#[test]
fn an_ambiguous_pairing_verifies_nothing() {
    let st = statement(&[(10, dec!(-150), dec!(-50)), (11, dec!(-150), dec!(-200))]);
    let existing =
        vec![held(1, dec!(100), true), unverified(9, dec!(-150), 1), unverified(10, dec!(-150), 2)];
    let err = with(&st, existing).expect_err("two records, two lines");
    assert!(err.to_string().contains("ambiguously"), "{err}");

    // One line, two records: still no choice.
    let st = statement(&[(10, dec!(-150), dec!(-50))]);
    let existing =
        vec![held(1, dec!(100), true), unverified(9, dec!(-150), 1), unverified(11, dec!(-150), 2)];
    assert!(with(&st, existing).is_err());
}

/// A record whose amount disagrees verifies nothing and breaks the chain.
#[test]
fn an_unverified_record_of_another_amount_breaks_the_chain() {
    let st = statement(&[(10, dec!(-20), dec!(80)), (20, dec!(5), dec!(85))]);
    let err = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-19), 42)])
        .expect_err("19 is not 20");
    assert!(err.to_string().contains("nothing was imported"), "{err}");
}

/// A spend recorded in the statement's last days that the bank books after
/// it: the record waits, unverified, on the day after the statement.
#[test]
fn a_record_from_the_last_days_waits_for_the_next_statement() -> Result<()> {
    let st = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let p = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-30), 42)])?;
    assert_eq!(p.deferred, vec![Deferred { transaction_id: 42, date: day(11) }]);
    assert_eq!((p.counts.deferred, p.counts.new, p.counts.verified), (1, 2, 0));
    Ok(())
}

/// One from earlier in the statement had its chance: the chain breaks.
#[test]
fn an_earlier_record_no_line_verifies_breaks_the_chain() {
    let st = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let err = with(&st, vec![held(1, dec!(100), true), unverified(2, dec!(-30), 42)])
        .expect_err("the statement never shows 30");
    assert!(err.to_string().contains("nothing was imported"), "{err}");
}

/// A transfer is one record with a leg on each account, and a line vouches
/// only for its own: the other bank's leg stays unverified until its own
/// statement shows it.
#[test]
fn a_line_leaves_the_other_banks_leg_of_a_transfer_unverified() -> Result<()> {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let record = LedgerPosting {
        other_accounts: vec![OTHER.to_string(), "Expenses:Food".to_string()],
        ..unverified(9, dec!(-20), 42)
    };
    let p = with(&st, vec![held(1, dec!(100), true), record])?;
    assert_eq!(p.verified[0].unchecked, [OTHER]);
    Ok(())
}

/// Moving a record moves its every leg, so one the other bank's statement
/// will be checked against is not this import's to move.
#[test]
fn a_transfer_the_statement_does_not_show_is_refused_rather_than_moved() {
    let st = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let record =
        LedgerPosting { other_accounts: vec![OTHER.to_string()], ..unverified(9, dec!(-30), 42) };
    let err = with(&st, vec![held(1, dec!(100), true), record])
        .expect_err("the far leg is not ours to move");
    assert!(err.to_string().contains(OTHER), "{err}");
    assert!(err.to_string().contains("Nothing was imported"), "{err}");
}

/// A record another bank's line already verified keeps that bank's date: the
/// two may have booked it on different days, and moving it would move the leg
/// that bank's statement is checked against.
#[test]
fn a_record_another_bank_already_verified_keeps_its_date() -> Result<()> {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let record = LedgerPosting {
        refs: vec!["tiantian:1".into(), "cathay-bank:20260609:1".into()],
        other_accounts: vec![OTHER.to_string()],
        ..unverified(9, dec!(-20), 42)
    };
    let p = with(&st, vec![held(1, dec!(100), true), record])?;
    assert_eq!(p.verified[0].date, None);
    assert_eq!(p.counts.verified, 1);
    Ok(())
}

/// Both banks booked the transfer on one day, and the other bank verified it
/// first: its ref is on the record, but this leg is still unverified, so the
/// line verifies it rather than taking it for a posting of its own.
#[test]
fn a_same_day_transfer_the_other_bank_verified_is_verified_here_too() -> Result<()> {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let record = LedgerPosting {
        refs: vec!["tiantian:1".into(), "cathay-bank:20260610:1".into()],
        other_accounts: vec![OTHER.to_string()],
        ..unverified(10, dec!(-20), 42)
    };
    let p = with(&st, vec![held(1, dec!(100), true), record])?;
    assert_eq!((p.counts.verified, p.counts.covered, p.counts.new), (1, 0, 0));
    assert_eq!(p.verified[0].unchecked, [OTHER]);
    assert_eq!(p.verified[0].date, None);
    Ok(())
}
