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
    statement_of("111111111111", lines)
}

fn statement_of(account_no: &str, lines: &[(u32, Decimal, Decimal)]) -> BankStatement {
    BankStatement {
        bank: Bank::Cathay,
        account_no: account_no.into(),
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
        id: i64::from(d),
        date: day(d),
        posted: None,
        amount,
        refs: Vec::new(),
        other_accounts: Vec::new(),
        opening,
        transaction_id: i64::from(d),
        unverified: false,
    }
}

/// `st` landing on `account`, which holds `existing`, all lines from one file.
fn on<'a>(account: &str, st: &'a BankStatement, existing: Vec<LedgerPosting>) -> Statement<'a> {
    Statement { account: account.into(), statement: st, files: vec![0; st.lines.len()], existing }
}

fn run(st: &BankStatement, existing: Vec<LedgerPosting>) -> Plan {
    plan(&chart(), &[on("Assets:Bank:Savings", st, existing)], &HashSet::new(), &[])
}

#[test]
fn a_new_account_gets_one_opening_before_its_first_line() {
    let st = statement(&[(2, dec!(-100), dec!(400)), (3, dec!(50), dec!(450))]);
    let p = run(&st, vec![]);
    assert_eq!(p.counts.openings, 1);
    assert_eq!(p.openings, vec![day(1)]);
    assert_eq!(p.transactions[0].1.postings[0].amount, dec!(500));
    assert_eq!(p.transactions.len(), 3);
}

/// Lines up to the opening are in its amount; later ones chain from it.
#[test]
fn lines_through_the_opening_date_are_skipped() {
    let st = statement(&[(1, dec!(100), dec!(100)), (3, dec!(5), dec!(105))]);
    let p = run(&st, vec![held(1, dec!(100), true)]);
    assert_eq!(p.counts.predate_opening, 1);
    assert_eq!(p.counts.new, 1);
    assert_eq!(p.counts.openings, 0);
}

/// With no opening, records carry the account from its first posting; they
/// must reach the statement's balance.
#[test]
fn records_without_an_opening_chain_from_their_first_posting() {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let p = run(&st, vec![held(1, dec!(100), false)]);
    let before_first_posting = day(1).pred_opt().expect("date");
    assert_eq!((p.openings, p.counts.new, p.counts.openings), (vec![before_first_posting], 1, 0));

    assert_eq!(run(&st, vec![held(1, dec!(90), false)]).gaps.len(), 1);
}

#[test]
fn a_later_opening_is_named_and_the_earliest_counts() {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let later = LedgerPosting { transaction_id: 77, date: day(2), ..held(2, dec!(0), true) };
    let p = run(&st, vec![held(1, dec!(100), true), later]);
    assert_eq!(p.openings, [day(1)]);
    assert_eq!(p.suggestions, [(77, Suggestion::ExtraOpening)]);
}

#[test]
fn a_ledger_that_disagrees_with_the_statement_is_booked_with_its_gap() {
    let st = statement(&[(3, dec!(5), dec!(105))]);
    let p = run(&st, vec![held(1, dec!(100), true), held(2, dec!(-1), false)]);
    assert_eq!(p.counts.new, 1);
    assert_eq!(p.gaps, [Gap {
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        day: day(3),
        ledger: dec!(104),
        statement: dec!(105),
    }]);
}

/// The account of the tests' second statement, which most of them leave out
/// of the import.
const OTHER: &str = "Assets:Bank:Other";

fn unverified(d: u32, amount: Decimal, id: i64) -> LedgerPosting {
    LedgerPosting { unverified: true, transaction_id: id, ..held(d, amount, false) }
}

fn with(st: &BankStatement, existing: Vec<LedgerPosting>) -> Plan {
    plan(&chart(), &[on("Assets:Bank:Savings", st, existing)], &HashSet::new(), &[])
}

/// A line verifies the one unverified record of its amount near it: the
/// record keeps its row and takes the line's date and ref; nothing is
/// inserted for the line.
#[test]
fn a_line_verifies_its_one_unverified_record_in_place() {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let p = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-20), 42)]);
    assert_eq!(p.verified, vec![Verification {
        transaction_id: 42,
        date: Some(day(10)),
        statement_ref: st.dedup_refs()[0].clone(),
        unchecked: Vec::new(),
    }]);
    assert_eq!((p.counts.verified, p.transactions.len()), (1, 0));
}

/// Two lunches of one price, two records of it: each line takes the nearest
/// record, and says so.
#[test]
fn an_ambiguous_pairing_takes_the_nearest() {
    let st = statement(&[(10, dec!(-150), dec!(-50)), (11, dec!(-150), dec!(-200))]);
    let existing = vec![
        held(1, dec!(100), true),
        unverified(9, dec!(-150), 41),
        unverified(10, dec!(-150), 42),
    ];
    let p = with(&st, existing);
    let verified: Vec<i64> = p.verified.iter().map(|v| v.transaction_id).collect();
    assert_eq!(verified, [42, 41]);
    assert_eq!(p.suggestions, [(42, Suggestion::PickedNearest), (41, Suggestion::PickedNearest)]);
    assert!(p.gaps.is_empty());
}

/// The record the line did not take waits for the next statement.
#[test]
fn the_record_a_line_left_waits_for_the_next_statement() {
    let st = statement(&[(10, dec!(-150), dec!(-50))]);
    let existing = vec![
        held(1, dec!(100), true),
        unverified(9, dec!(-150), 41),
        unverified(5, dec!(-150), 42),
    ];
    let p = with(&st, existing);
    assert_eq!(p.verified[0].transaction_id, 41);
    assert_eq!(p.deferred[0].transaction_id, 42);
}

/// Uncategorised lines say how their other leg was chosen.
#[test]
fn a_fallback_leg_carries_its_origin() {
    let st = statement(&[(2, dec!(-100), dec!(400))]);
    let p = run(&st, vec![]);
    let line = &p.transactions[1].1;
    assert_eq!(line.postings[1].origin, Some(Origin::Fallback));
    assert_eq!(line.postings[0].origin, None);
}

/// A record whose amount disagrees verifies nothing: the statement passes
/// it by, and the import says which record, not just where the chain broke.
#[test]
fn an_unverified_record_of_another_amount_is_not_on_the_statement() {
    let st = statement(&[(10, dec!(-20), dec!(80)), (20, dec!(5), dec!(85))]);
    let p = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-19), 42)]);
    assert!(matches!(
        p.suggestions.as_slice(),
        [(42, Suggestion::NotOnStatement { siblings, .. })] if siblings.is_empty()
    ));
    assert_eq!(p.counts.new, 2);
}

/// A spend recorded in the statement's last days that the bank books after
/// it: the record waits, unverified, on the day after the statement.
#[test]
fn a_record_from_the_last_days_waits_for_the_next_statement() {
    let st = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let p = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-30), 42)]);
    assert_eq!(p.deferred, vec![Deferred { transaction_id: 42, date: day(11) }]);
    assert_eq!((p.counts.deferred, p.counts.new, p.counts.verified), (1, 2, 0));
}

/// The app records the pooled accounts only together, so a record may be on
/// the wrong one: with one sibling line that fits it, the record moves there
/// and that line verifies it.
#[test]
fn a_record_its_statement_passes_by_moves_to_the_sibling_that_shows_it() {
    let savings = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let other = statement_of("222222222222", &[(3, dec!(-30), dec!(70))]);
    let statements = [
        on("Assets:Bank:Savings", &savings, vec![
            held(1, dec!(100), true),
            unverified(2, dec!(-30), 42),
        ]),
        on(OTHER, &other, vec![held(1, dec!(100), true)]),
    ];
    let p = plan(&chart(), &statements, &HashSet::new(), &[]);
    assert_eq!(p.moved, [Moved {
        transaction_id: 42,
        from: "Assets:Bank:Savings".into(),
        to: OTHER.into(),
        amount: dec!(-30),
        date: day(2),
    }]);
    assert_eq!((p.verified[0].transaction_id, p.verified[0].date), (42, Some(day(3))));
    assert_eq!((p.counts.moved, p.counts.new), (1, 2));
}

/// Two such lines, and nothing chooses: the import stops naming the sibling.
#[test]
fn a_record_two_sibling_lines_fit_names_the_sibling() {
    let savings = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let other = statement_of("222222222222", &[(3, dec!(-30), dec!(70)), (4, dec!(-30), dec!(40))]);
    let statements = [
        on("Assets:Bank:Savings", &savings, vec![
            held(1, dec!(100), true),
            unverified(2, dec!(-30), 42),
        ]),
        on(OTHER, &other, vec![held(1, dec!(100), true)]),
    ];
    let p = plan(&chart(), &statements, &HashSet::new(), &[]);
    assert!(matches!(
        p.suggestions.as_slice(),
        [(42, Suggestion::NotOnStatement { siblings, .. })] if siblings == &[OTHER]
    ));
}

/// Two statements' lines verifying one transfer on different days.
fn two_dated(other: &[(u32, Decimal, Decimal)]) -> Plan {
    let savings = statement(&[(10, dec!(-20), dec!(80))]);
    let other = statement_of("222222222222", other);
    let transfer = |amount, other_account: &str| LedgerPosting {
        other_accounts: vec![other_account.to_string()],
        ..unverified(9, amount, 42)
    };
    let statements = [
        on("Assets:Bank:Savings", &savings, vec![
            held(1, dec!(100), true),
            transfer(dec!(-20), OTHER),
        ]),
        on(OTHER, &other, vec![
            held(1, dec!(100), true),
            transfer(dec!(20), "Assets:Bank:Savings"),
        ]),
    ];
    plan(&chart(), &statements, &HashSet::new(), &[])
}

/// A transfer the banks booked a day apart (near midnight) stays one record,
/// on the day both statements' balances agree with.
#[test]
fn a_transfer_two_banks_date_apart_keeps_one_date() {
    let p = two_dated(&[(11, dec!(20), dec!(120))]);
    let dates: Vec<_> = p.verified.iter().map(|v| (v.transaction_id, v.date)).collect();
    assert_eq!(dates, [(42, Some(day(10))), (42, Some(day(10)))]);
}

/// When neither day keeps both statements' balances, it still stays one
/// record, on the earlier day.
#[test]
fn a_transfer_no_one_date_balances_stays_one_on_the_earliest() {
    let p = two_dated(&[(10, dec!(5), dec!(105)), (11, dec!(20), dec!(125))]);
    let dates: Vec<_> = p.verified.iter().map(|v| v.date).collect();
    assert_eq!(dates, [Some(day(10)), Some(day(10))]);
    assert_eq!(p.suggestions, [(42, Suggestion::OneDate {
        kept: day(10),
        dates: vec![("Assets:Bank:Savings".into(), day(10)), (OTHER.into(), day(11))],
    })]);
}

/// A transfer is one record with a leg on each account, and a line vouches
/// only for its own: the other bank's leg stays unverified until its own
/// statement shows it.
#[test]
fn a_line_leaves_the_other_banks_leg_of_a_transfer_unverified() {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let record = LedgerPosting {
        other_accounts: vec![OTHER.to_string(), "Expenses:Food".to_string()],
        ..unverified(9, dec!(-20), 42)
    };
    let p = with(&st, vec![held(1, dec!(100), true), record]);
    assert_eq!(p.verified[0].unchecked, [OTHER]);
}

/// Moving a record moves its every leg, so one the other bank's statement
/// will be checked against stays where it is, and says why.
#[test]
fn a_transfer_the_statement_does_not_show_stays_rather_than_moves() {
    let st = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let record =
        LedgerPosting { other_accounts: vec![OTHER.to_string()], ..unverified(9, dec!(-30), 42) };
    let p = with(&st, vec![held(1, dec!(100), true), record]);
    assert_eq!(p.suggestions, [(42, Suggestion::SharedDeferral { other: OTHER.into() })]);
    assert!(p.deferred.is_empty());
}

/// A record another bank's line already verified keeps that bank's date: the
/// two may have booked it on different days, and moving it would move the leg
/// that bank's statement is checked against.
#[test]
fn a_record_another_bank_already_verified_keeps_its_date() {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let record = LedgerPosting {
        refs: vec!["tiantian:1".into(), "cathay-bank:20260609:1".into()],
        other_accounts: vec![OTHER.to_string()],
        ..unverified(9, dec!(-20), 42)
    };
    let p = with(&st, vec![held(1, dec!(100), true), record]);
    assert_eq!(p.verified[0].date, None);
    assert_eq!(p.counts.verified, 1);
}

/// Both banks booked the transfer on one day, and the other bank verified it
/// first: its ref is on the record, but this leg is still unverified, so the
/// line verifies it rather than taking it for a posting of its own.
#[test]
fn a_same_day_transfer_the_other_bank_verified_is_verified_here_too() {
    let st = statement(&[(10, dec!(-20), dec!(80))]);
    let record = LedgerPosting {
        refs: vec!["tiantian:1".into(), "cathay-bank:20260610:1".into()],
        other_accounts: vec![OTHER.to_string()],
        ..unverified(10, dec!(-20), 42)
    };
    let p = with(&st, vec![held(1, dec!(100), true), record]);
    assert_eq!((p.counts.verified, p.counts.covered, p.counts.new), (1, 0, 0));
    assert_eq!(p.verified[0].unchecked, [OTHER]);
    assert_eq!(p.verified[0].date, None);
}
