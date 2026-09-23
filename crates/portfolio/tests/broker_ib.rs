//! IB activity statement quirks, on invented statements.

use portfolio::broker::{ib, replay, Commodity, RecordKind};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// An invented statement: the given section rows between a minimal header
/// and the closing figures.
fn statement(
    period: &str,
    generated: &str,
    body: &str,
    ending_cash: &str,
    positions: &str,
) -> String {
    format!(
        "\u{feff}Statement,Header,Field Name,Field Value
Statement,Data,Title,Activity Statement
Statement,Data,Period,\"{period}\"
Statement,Data,WhenGenerated,\"{generated}\"
Account Information,Header,Field Name,Field Value
Account Information,Data,Account,U0000000 (Custom Consolidated)
Account Information,Data,Base Currency,USD
Cash Report,Header,Currency Summary,Currency,Total,Securities,Futures,
Cash Report,Data,Starting Cash,Base Currency Summary,0,0,0,
Cash Report,Data,Ending Cash,Base Currency Summary,{ending_cash},{ending_cash},0,
Open Positions,Header,DataDiscriminator,Asset Category,Currency,Symbol,Open,Quantity,Mult,Cost \
         Price,Cost Basis,Close Price,Value,Unrealized P/L,Code
{positions}Open Positions,Total,,Stocks,USD,,,,,,0,,0,0,
{body}"
    )
}

const TRADES: &str = "Trades,Header,DataDiscriminator,Asset \
                      Category,Currency,Account,Symbol,Date/Time,Quantity,T. Price,C. \
                      Price,Proceeds,Comm/Fee,Basis,Realized P/L,MTM P/L,Code\n";
const DEPOSITS: &str =
    "Deposits & Withdrawals,Header,Currency,Account,Settle Date,Description,Amount\n";
const WITHHOLDING: &str = "Withholding Tax,Header,Currency,Account,Date,Description,Amount,Code\n";
const GRANTS: &str = "Grant Activity,Header,Account,Symbol,Report Date,Description,Award \
                      Date,Vesting Date,Quantity,Price,Value\n";
const ACTIONS: &str = "Corporate Actions,Header,Asset Category,Currency,Account,Report \
                       Date,Date/Time,Description,Quantity,Proceeds,Value,Realized P/L,Code\n";

#[test]
fn a_consolidated_statement_is_keyed_on_the_account_number() {
    let s = ib::parse(&statement("January 1, 2025 - December 31, 2025", "2026-01-08, 22:56:25 EST", DEPOSITS, "0", "")).unwrap();
    assert_eq!(s.account, "U0000000");
}

#[test]
fn a_withholding_reversal_pair_stays_three_records() {
    let body = format!(
        "{WITHHOLDING}Withholding Tax,Data,USD,U0000000,2025-02-13,ZZA(US0000000001) Cash \
         Dividend USD 0.25 per Share - US Tax,-1.13,
Withholding Tax,Data,USD,U0000000,2025-02-13,ZZA(US0000000001) Cash Dividend USD 0.25 per Share - \
         US Tax,1.13,
Withholding Tax,Data,USD,U0000000,2025-02-13,ZZA(US0000000001) Cash Dividend USD 0.25 per Share - \
         US Tax,-1.13,
Withholding Tax,Data,Total,,,,-1.13,
"
    );
    let s = ib::parse(&statement(
        "January 1, 2025 - December 31, 2025",
        "2026-01-08, 22:56:25 EST",
        &body,
        "-1.13",
        "",
    ))
    .unwrap();
    let amounts: Vec<Decimal> = s.records.iter().map(|r| r.amount).collect();
    assert_eq!(amounts, [dec!(-1.13), dec!(1.13), dec!(-1.13)]);
    assert!(s.records.iter().all(|r| r.kind == RecordKind::Withholding));
    assert!(s.records.iter().all(|r| r.symbol.as_deref() == Some("ZZA")));

    // The two identical charges keep distinct keys.
    let mut keys: Vec<&str> = s.records.iter().map(|r| r.key.as_str()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 3);
    assert!(s.records[2].key.ends_with("#2"), "{}", s.records[2].key);
}

#[test]
fn identical_trades_are_numbered_and_times_become_utc() {
    let row = "Trades,Data,Order,Stocks,USD,U0000000,ZZB,\"2025-07-10, \
               09:30:00\",1,10,10,-10,-1,11,0,0,O\n";
    let body = format!("{TRADES}{row}{row}Trades,SubTotal,,Stocks,USD,ZZB,,2,,,-20,-2,22,0,0,\n");
    let s = ib::parse(&statement(
        "January 1, 2025 - December 31, 2025",
        "2026-01-08, 22:56:25 EST",
        &body,
        "-22",
        "",
    ))
    .unwrap();
    assert_eq!(s.records.len(), 2);
    assert_ne!(s.records[0].key, s.records[1].key);
    assert_eq!(s.records[0].executed_at.unwrap().to_string(), "2025-07-10 13:30:00 UTC");
    assert_eq!(s.records[0].cash(), dec!(-11));
}

#[test]
fn vesting_moves_no_shares_and_withholding_removes_them() {
    let body = format!(
        "{GRANTS}Grant Activity,Data,U0000000,ZZC,2025-01-30,Stock Award Grant for Cash \
         Deposit,2025-01-30,2026-01-30,2,10,20
Grant Activity,Data,U0000000,ZZC,2026-01-30,Stock Award Vesting,2025-01-30,2026-01-30,2,12,24
Grant Activity,Data,U0000000,ZZC,2026-01-30,Stock Award \
         Withholding,2025-01-30,2026-01-30,-0.6,12,-7.2
Grant Activity,Data,Total,,,,,,3.4,,36.8
"
    );
    let s = ib::parse(&statement(
        "January 1, 2025 - September 21, 2026",
        "2026-09-22, 00:54:17 EDT",
        &body,
        "0",
        "",
    ))
    .unwrap();
    let kinds: Vec<RecordKind> = s.records.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, [
        RecordKind::AwardGrant,
        RecordKind::AwardVesting,
        RecordKind::AwardWithholding
    ]);
    let end = replay(&s.records, s.period_end);
    assert_eq!(end.get(&Commodity::Security("ZZC".into())), Some(&dec!(1.4)));
    assert!(end.keys().all(|c| !matches!(c, Commodity::Cash(_))), "grants move no cash: {end:?}");
}

#[test]
fn a_split_comes_from_corporate_actions_on_its_report_date() {
    let body = format!(
        "{ACTIONS}Corporate Actions,Data,Stocks,USD,U0000000,2025-06-18,\"2025-06-17, \
         20:25:00\",\"ZZD(US0000000004) Split 4 for 1 (ZZD, INVENTED CO, US0000000004)\",3,0,0,0,
Corporate Actions,Data,Total,,,,,,0,0,0,
"
    );
    let s = ib::parse(&statement(
        "January 1, 2025 - December 31, 2025",
        "2026-01-08, 22:56:25 EST",
        &body,
        "0",
        "",
    ))
    .unwrap();
    let [split] = s.records.as_slice() else { panic!("{:?}", s.records) };
    assert_eq!(split.kind, RecordKind::Split);
    assert_eq!(split.settle_date.to_string(), "2025-06-18");
    assert_eq!(split.quantity, dec!(3));
}

#[test]
fn closing_positions_skip_lots_and_totals() {
    let positions = "Open Positions,Data,Summary,Stocks,USD,ZZE,-,5,1,1,5,1,5,0,
Open Positions,Data,Lot,Stocks,USD,ZZE,2025-04-15,5,,1,5,1,5,
";
    let s = ib::parse(&statement(
        "January 1, 2025 - December 31, 2025",
        "2026-01-08, 22:56:25 EST",
        DEPOSITS,
        "0",
        positions,
    ))
    .unwrap();
    let closing = s.closing.unwrap();
    assert_eq!(closing.positions.len(), 1);
    assert_eq!(closing.positions["ZZE"], dec!(5));
    assert_eq!(closing.cash[&ledger_types::currency::Currency::USD], Decimal::ZERO);
}

#[test]
fn deposits_replay_to_the_ending_cash() {
    let body = format!(
        "{DEPOSITS}Deposits & Withdrawals,Data,USD,U0000000,2025-01-28,ACATS Transfer In From \
         Account 00000000,-75
Deposits & Withdrawals,Data,USD,U0000000,2025-01-28,ACATS Transfer In From Account 00000000,74.22
Deposits & Withdrawals,Data,USD,U0000000,2025-04-10,Electronic Fund Transfer,100
Deposits & Withdrawals,Data,Total,,,,99.22
"
    );
    let s = ib::parse(&statement(
        "January 1, 2025 - December 31, 2025",
        "2026-01-08, 22:56:25 EST",
        &body,
        "99.22",
        "",
    ))
    .unwrap();
    assert_eq!(s.records.len(), 3, "the Total row is not a record");
    let cash =
        replay(&s.records, s.period_end)[&Commodity::Cash(ledger_types::currency::Currency::USD)];
    assert_eq!(Some(cash), s.closing.unwrap().cash.values().next().copied());
}

#[test]
fn an_unknown_corporate_action_fails_loudly() {
    let body = format!(
        "{ACTIONS}Corporate Actions,Data,Stocks,USD,U0000000,2025-06-18,\"2025-06-17, \
         20:25:00\",ZZF(US0000000006) Merged(Acquisition) FOR USD 1 PER SHARE,-3,3,0,0,
"
    );
    let err = ib::parse(&statement(
        "January 1, 2025 - December 31, 2025",
        "2026-01-08, 22:56:25 EST",
        &body,
        "0",
        "",
    ))
    .unwrap_err();
    assert!(format!("{err:#}").contains("only splits"), "{err:#}");
}

#[test]
fn the_tracker_keeps_each_withholding_line_and_skips_vesting() {
    let body = format!(
        "{WITHHOLDING}Withholding Tax,Data,USD,U0000000,2025-02-13,ZZA(US0000000001) Cash \
         Dividend - US Tax,-1.13,
Withholding Tax,Data,USD,U0000000,2025-02-13,ZZA(US0000000001) Cash Dividend - US Tax,1.13,
Withholding Tax,Data,USD,U0000000,2025-02-13,ZZA(US0000000001) Cash Dividend - US Tax,-1.13,
{GRANTS}Grant Activity,Data,U0000000,ZZC,2025-01-30,Stock Award Grant for Cash \
         Deposit,2025-01-30,2026-01-30,2,10,20
Grant Activity,Data,U0000000,ZZC,2025-03-30,Stock Award Vesting,2025-01-30,2026-01-30,2,12,24
{ACTIONS}Corporate Actions,Data,Stocks,USD,U0000000,2025-06-18,\"2025-06-17, \
         20:25:00\",\"ZZC(US0000000003) Split 4 for 1 (ZZC, INVENTED CO, US0000000003)\",6,0,0,0,
"
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        statement(
            "January 1, 2025 - December 31, 2025",
            "2026-01-08, 22:56:25 EST",
            &body,
            "-1.13",
            "",
        ),
    )
    .unwrap();
    let (events, _) = portfolio::ib::load_from_csv(file.path().to_str().unwrap()).unwrap();
    let taxes: Vec<Decimal> = events
        .values()
        .flat_map(|e| &e.transactions)
        .filter(|t| t.kind == portfolio::portfolio::TransactionKind::Tax)
        .map(|t| t.amount)
        .collect();
    assert_eq!(taxes, [dec!(-1.13), dec!(1.13), dec!(-1.13)]);
    let deposits: Vec<Decimal> = events
        .values()
        .flat_map(|e| &e.transactions)
        .filter(|t| t.kind == portfolio::portfolio::TransactionKind::Deposit)
        .map(|t| t.quantity)
        .collect();
    assert_eq!(deposits, [dec!(2)], "vesting is not a second deposit");
    let splits: Vec<_> = events.values().flat_map(|e| e.splits.clone()).collect();
    assert_eq!(splits, [("ZZC".to_string(), dec!(4))]);
}
