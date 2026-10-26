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
        date: day(d),
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
    Statement {
        account: account.into(),
        statement: st,
        files: vec![0; st.lines.len()],
        existing,
        chosen: HashMap::new(),
    }
}

fn run(st: &BankStatement, existing: Vec<LedgerPosting>) -> Result<Plan> {
    plan(&chart(), &[on("Assets:Bank:Savings", st, existing)], &HashSet::new(), &[])
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

/// What a refused plan stopped on.
fn stops(err: anyhow::Error) -> Vec<Stop> {
    match err.downcast::<Refused>() {
        Ok(Refused(stops)) => stops,
        Err(err) => panic!("not a typed refusal: {err}"),
    }
}

fn with(st: &BankStatement, existing: Vec<LedgerPosting>) -> Result<Plan> {
    picked(st, existing, HashMap::new())
}

fn picked(
    st: &BankStatement,
    existing: Vec<LedgerPosting>,
    chosen: HashMap<String, i64>,
) -> Result<Plan> {
    let s = Statement { chosen, ..on("Assets:Bank:Savings", st, existing) };
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
    assert!(stops(err).iter().all(|s| matches!(s, Stop::LineAmbiguous(_))));
}

/// The refusal names each line and the records it fits, for the queue.
#[test]
fn an_ambiguity_names_its_lines_and_records() {
    let st = statement(&[(10, dec!(-150), dec!(-50))]);
    let existing =
        vec![held(1, dec!(100), true), unverified(9, dec!(-150), 1), unverified(11, dec!(-150), 2)];
    let err = with(&st, existing).expect_err("one line, two records");
    assert_eq!(stops(err), [Stop::LineAmbiguous(Ambiguity {
        account: "Assets:Bank:Savings".into(),
        currency: Currency::TWD,
        statement_ref: st.dedup_refs()[0].clone(),
        date: day(10),
        amount: dec!(-150),
        description: String::new(),
        candidates: vec![1, 2],
    })]);
}

/// A person's pick settles it: the picked record is verified, the other
/// stays unverified, and the second line pairs with the one record left.
#[test]
fn a_pick_pairs_what_the_amounts_could_not() -> Result<()> {
    let st = statement(&[(10, dec!(-150), dec!(-50)), (11, dec!(-150), dec!(-200))]);
    let existing = vec![
        held(1, dec!(100), true),
        unverified(9, dec!(-150), 41),
        unverified(10, dec!(-150), 42),
    ];
    let refs = st.dedup_refs();
    let p = picked(&st, existing, HashMap::from([(refs[0].clone(), 42)]))?;
    let verified: Vec<_> =
        p.verified.iter().map(|v| (v.statement_ref.as_str(), v.transaction_id)).collect();
    assert_eq!(verified, [(refs[0].as_str(), 42), (refs[1].as_str(), 41)]);
    assert_eq!((p.counts.verified, p.transactions.len()), (2, 0));

    // A pick naming a record no longer unverified is no pick.
    let existing = vec![
        held(1, dec!(100), true),
        unverified(9, dec!(-150), 41),
        unverified(10, dec!(-150), 42),
    ];
    let stale = picked(&st, existing, HashMap::from([(refs[0].clone(), 99)]));
    let stale = stops(stale.expect_err("still ambiguous"));
    assert!(matches!(stale.as_slice(), [Stop::LineAmbiguous(_), ..]), "{stale:?}");
    Ok(())
}

/// A pick is checked against its line again: a record edited since to
/// another amount no longer fits, and the line pairs as if never picked.
#[test]
fn a_pick_whose_record_no_longer_fits_is_ignored() -> Result<()> {
    let st = statement(&[(10, dec!(-150), dec!(-50))]);
    let existing = vec![
        held(1, dec!(100), true),
        unverified(9, dec!(-150), 41),
        unverified(8, dec!(-140), 42),
    ];
    let refs = st.dedup_refs();
    let p = picked(&st, existing, HashMap::from([(refs[0].clone(), 42)]))?;
    let verified: Vec<_> = p.verified.iter().map(|v| v.transaction_id).collect();
    assert_eq!(verified, [41]);
    Ok(())
}

/// Every stop is reported at once, so the queue shows them together: an
/// ambiguous line beside another bank's record the statement leaves unshown.
/// The records the line may be are not reported missing.
#[test]
fn an_ambiguity_is_reported_beside_a_record_left_to_the_next_statement() {
    let st = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-150), dec!(-45))]);
    let shared =
        LedgerPosting { other_accounts: vec![OTHER.to_string()], ..unverified(9, dec!(-30), 40) };
    let existing = vec![
        held(1, dec!(100), true),
        unverified(8, dec!(-150), 41),
        unverified(9, dec!(-150), 42),
        shared,
    ];
    let found = stops(with(&st, existing).expect_err("ambiguous"));
    let [Stop::LineAmbiguous(line), Stop::SharedDeferral { transaction_id: 40, .. }] =
        found.as_slice()
    else {
        panic!("{found:?}")
    };
    assert_eq!(line.candidates, [41, 42]);
}

/// Uncategorised lines say how their other leg was chosen.
#[test]
fn a_fallback_leg_carries_its_origin() -> Result<()> {
    let st = statement(&[(2, dec!(-100), dec!(400))]);
    let p = run(&st, vec![])?;
    let line = &p.transactions[1].1;
    assert_eq!(line.postings[1].origin, Some(Origin::Fallback));
    assert_eq!(line.postings[0].origin, None);
    Ok(())
}

/// A record whose amount disagrees verifies nothing: the statement passes
/// it by, and the import says which record, not just where the chain broke.
#[test]
fn an_unverified_record_of_another_amount_is_not_on_the_statement() {
    let st = statement(&[(10, dec!(-20), dec!(80)), (20, dec!(5), dec!(85))]);
    let err = with(&st, vec![held(1, dec!(100), true), unverified(9, dec!(-19), 42)])
        .expect_err("19 is not 20");
    assert!(
        matches!(stops(err).as_slice(), [Stop::NotOnStatement { transaction_id: 42, siblings, .. }]
            if siblings.is_empty())
    );
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

/// The app records the pooled accounts only together, so a record may be on
/// the wrong one: with one sibling line that fits it, the record moves there
/// and that line verifies it.
#[test]
fn a_record_its_statement_passes_by_moves_to_the_sibling_that_shows_it() -> Result<()> {
    let savings = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let other = statement_of("222222222222", &[(3, dec!(-30), dec!(70))]);
    let statements = [
        on("Assets:Bank:Savings", &savings, vec![
            held(1, dec!(100), true),
            unverified(2, dec!(-30), 42),
        ]),
        on(OTHER, &other, vec![held(1, dec!(100), true)]),
    ];
    let p = plan(&chart(), &statements, &HashSet::new(), &[])?;
    assert_eq!(p.moved, [Moved {
        transaction_id: 42,
        from: "Assets:Bank:Savings".into(),
        to: OTHER.into(),
        amount: dec!(-30),
        date: day(2),
    }]);
    assert_eq!((p.verified[0].transaction_id, p.verified[0].date), (42, Some(day(3))));
    assert_eq!((p.counts.moved, p.counts.new), (1, 2));
    Ok(())
}

/// Two such lines, and nothing chooses: the import stops naming the sibling.
#[test]
fn a_record_two_sibling_lines_fit_stops_the_import() {
    let savings = statement(&[(3, dec!(5), dec!(105)), (10, dec!(-20), dec!(85))]);
    let other = statement_of("222222222222", &[(3, dec!(-30), dec!(70)), (4, dec!(-30), dec!(40))]);
    let statements = [
        on("Assets:Bank:Savings", &savings, vec![
            held(1, dec!(100), true),
            unverified(2, dec!(-30), 42),
        ]),
        on(OTHER, &other, vec![held(1, dec!(100), true)]),
    ];
    let err = plan(&chart(), &statements, &HashSet::new(), &[]).expect_err("two lines of 30");
    assert!(
        matches!(stops(err).as_slice(), [Stop::NotOnStatement { siblings, .. }] if siblings == &[OTHER])
    );
}

/// Two statements' lines verifying one transfer on different days.
fn two_dated(other: &[(u32, Decimal, Decimal)]) -> Result<Plan> {
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
fn a_transfer_two_banks_date_apart_keeps_one_date() -> Result<()> {
    let p = two_dated(&[(11, dec!(20), dec!(120))])?;
    let dates: Vec<_> = p.verified.iter().map(|v| (v.transaction_id, v.date)).collect();
    assert_eq!(dates, [(42, Some(day(10))), (42, Some(day(10)))]);
    Ok(())
}

/// When neither day keeps both statements' balances, the import stops.
#[test]
fn a_transfer_no_one_date_balances_stops_the_import() {
    let err = two_dated(&[(10, dec!(5), dec!(105)), (11, dec!(20), dec!(125))])
        .expect_err("neither day balances");
    assert_eq!(stops(err), [Stop::TwoDates {
        transaction_id: 42,
        dates: vec![("Assets:Bank:Savings".into(), day(10)), (OTHER.into(), day(11))],
    }]);
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
    let found = stops(err);
    assert!(
        matches!(found.as_slice(), [Stop::SharedDeferral { transaction_id: 42, other, .. }]
            if other == OTHER),
        "{found:?}"
    );
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
