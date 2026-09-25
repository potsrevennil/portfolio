//! Firstrade statement quirks, on invented `pdftotext -layout` text.

use std::collections::HashMap;

use chrono::NaiveDate;
use ledger_types::currency::Currency;
use portfolio::broker::{firstrade, replay, BrokerStatement, Commodity, RecordKind};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn header() -> String {
    format!(
        "{:<17}{:<10}{:<8}{:<30}{:>10}{:>12}{:>12}{:>12}",
        "TRANSACTION", "DATE", "TYPE", "DESCRIPTION", "QUANTITY", "PRICE", "DEBIT", "CREDIT"
    )
}

/// One activity line with its amount under the debit or credit column.
fn row(
    tx: &str,
    date: &str,
    ty: &str,
    desc: &str,
    qty: &str,
    price: &str,
    debit: &str,
    credit: &str,
) -> String {
    format!("{tx:<17}{date:<9} {ty:<8}{desc:<30}{qty:>10}{price:>12}{debit:>12}{credit:>12}")
}

fn more(text: &str) -> String { format!("{:<35}{text}", "") }

fn statement(
    period: &str,
    open: &str,
    close: &str,
    priced: &str,
    holdings: &[&str],
    activity: &[String],
) -> String {
    let mut lines = vec![
        format!("    {period}"),
        "    ACCOUNT NUMBER          000-00000-00 RR WWW".to_string(),
        "                         OPENING BALANCE    CLOSING BALANCE".to_string(),
        format!("    NET ACCOUNT BALANCE     {open}     {close}"),
        format!("    TOTAL PRICED PORTFOLIO  {priced}"),
        "      DESCRIPTION            SYMBOL/   ACCOUNT".to_string(),
        "                             CUSIP     TYPE   QUANTITY   PRICE   MARKET VALUE".to_string(),
    ];
    lines.extend(holdings.iter().map(|h| h.to_string()));
    lines.push("      TOTAL PRICED PORTFOLIO                                  $0".to_string());
    lines.push(header());
    lines.extend(activity.iter().cloned());
    lines.join("\n")
}

fn june() -> String {
    statement(
        "April 1, 2024 - June 30, 2024",
        "10.00",
        "736.99",
        "0.00     300.10",
        &["    * INVENTED A CORP        ZZA       M          3.001      $100.00     $300.10"],
        &[
            "    BUY / SELL TRANSACTIONS".to_string(),
            row("BOUGHT", "06/13/24", "M", "INVENTED A CORP", "5", "$100", "$500.00", ""),
            more("CUSIP: 000000001"),
            row("SOLD", "06/20/24", "M", "INVENTED A CORP", "2", "101.0003", "", "201.99"),
            more("CUSIP: 000000001"),
            "    Total Buy / Sell Transactions                               $500.00    $201.99"
                .to_string(),
            "    DIVIDENDS AND INTEREST".to_string(),
            row("REINVEST", "06/25/24", "M", "INVENTED A CORP", "0.001", "", "0.52", ""),
            more("REIN @ 520.0000"),
            more("CUSIP: 000000001"),
            row("DIVIDEND", "06/25/24", "M", "INVENTED A CORP", "", "0.25", "", "0.75"),
            more("CASH DIV ON 3 SHARES                    WH      0.23"),
            more("CUSIP: 000000001"),
            "    Total Dividends And Interest                                 $0.75      $0.75"
                .to_string(),
            "    FUNDS PAID AND RECEIVED".to_string(),
            row("WIRE", "06/11/24", "M", "Wire Funds Received", "", "", "", "$1,000.00"),
            "    Total Funds Paid And Received                                        $1,000.00"
                .to_string(),
            "    MISCELLANEOUS TRANSACTIONS".to_string(),
            row("CSH", "06/17/24", "C", "XFER CASH TO MARGIN", "", "", "$25.00", ""),
            row("FEE", "06/17/24", "C", "rebate for wire", "", "", "", "25.00"),
            row("CSH", "06/17/24", "M", "XFER MARGIN TO CASH", "", "", "", "25.00"),
            "    Total Miscellaneous Transactions                             $25.00     $50.00"
                .to_string(),
            row("BOUGHT", "06/28/24 07/01/24", "M", "INVENTED B CORP", "1", "$50", "$50.00", ""),
            "    Total Executed Trades Pending Settlement                     $50.00".to_string(),
        ],
    )
}

fn july() -> String {
    statement(
        "July 1, 2024 - July 31, 2024",
        "736.99",
        "686.99",
        "300.10     50.00",
        &["    * INVENTED B CORP        ZZB       M          1          $50.00      $50.00"],
        &[
            row("BOUGHT", "07/01/24", "M", "INVENTED B CORP", "1", "$50", "$50.00", ""),
            "    Total Buy / Sell Transactions                               $50.00".to_string(),
            row("TFO", "07/30/24", "M", "INVENTED A CORP", "-3.001", "", "", ""),
            more("TRANSFER TO ANOTHER BROKER"),
            more("CUSIP: 000000001"),
            "    Total Securities Received And Delivered".to_string(),
        ],
    )
}

fn parse(texts: &[String]) -> Vec<BrokerStatement> {
    firstrade::parse_all(texts, &HashMap::new()).unwrap()
}

#[test]
fn every_statement_replays_to_its_own_closing() {
    let statements = parse(&[june(), july()]);
    let mut records = statements[0].opening_records();
    records.extend(statements.iter().flat_map(|s| s.records.clone()));
    for s in &statements {
        let replayed = replay(&records, s.period_end);
        for (commodity, stated) in s.closing.as_ref().unwrap().iter() {
            assert_eq!(
                replayed.get(&commodity).copied().unwrap_or_default(),
                stated,
                "{commodity} at {}",
                s.period_end
            );
        }
    }
    let end = replay(&records, statements[1].period_end);
    assert_eq!(end[&Commodity::Security("ZZA".into())], Decimal::ZERO, "transferred out");
}

#[test]
fn the_first_statement_opens_on_the_day_before_its_period() {
    let statements = parse(&[june()]);
    let [opening] = statements[0].opening_records().try_into().unwrap();
    assert_eq!(opening.kind, RecordKind::Opening);
    assert_eq!(opening.settle_date, NaiveDate::from_ymd_opt(2024, 3, 31).unwrap());
    assert_eq!(opening.cash(), dec!(10));
    // July opens holding securities it doesn't list: no stated opening.
    assert!(parse(&[june(), july()])[1].opening.is_none());
}

#[test]
fn a_sale_is_printed_net_of_the_sec_fee() {
    let s = &parse(&[june()])[0];
    let sale = s.records.iter().find(|r| r.kind == RecordKind::Sell).unwrap();
    assert_eq!(
        (sale.quantity, sale.amount, sale.commission),
        (dec!(-2), dec!(202.00), dec!(-0.01))
    );
    assert_eq!(sale.symbol.as_deref(), Some("ZZA"));
}

#[test]
fn a_dividend_and_its_withholding_are_two_records() {
    let s = &parse(&[june()])[0];
    let dividend: Vec<_> = s
        .records
        .iter()
        .filter(|r| matches!(r.kind, RecordKind::Dividend | RecordKind::Withholding))
        .map(|r| (r.kind, r.amount))
        .collect();
    assert_eq!(dividend, [
        (RecordKind::Dividend, dec!(0.75)),
        (RecordKind::Withholding, dec!(-0.23))
    ]);
    let rein = s.records.iter().find(|r| r.description.contains("reinvestment")).unwrap();
    assert_eq!((rein.kind, rein.quantity, rein.price), (RecordKind::Buy, dec!(0.001), dec!(520)));
    assert_eq!(rein.trade_date, Some(rein.settle_date));
}

#[test]
fn a_pending_trade_dates_the_line_that_settles_it() {
    let statements = parse(&[june(), july()]);
    let buy = statements[1].records.iter().find(|r| r.kind == RecordKind::Buy).unwrap();
    assert_eq!(buy.trade_date, NaiveDate::from_ymd_opt(2024, 6, 28));
    assert_eq!(buy.settle_date, NaiveDate::from_ymd_opt(2024, 7, 1).unwrap());
    let june_buy = statements[0]
        .records
        .iter()
        .find(|r| r.kind == RecordKind::Buy && r.quantity == dec!(5))
        .unwrap();
    assert_eq!(june_buy.trade_date, None, "settled lines don't print their trade date");
    assert!(
        statements[0].records.iter().all(|r| r.symbol.as_deref() != Some("ZZB")),
        "pending is not settled"
    );
}

#[test]
fn sweeps_are_kept_and_net_to_zero() {
    let s = &parse(&[june()])[0];
    let sweeps: Vec<Decimal> =
        s.records.iter().filter(|r| r.kind == RecordKind::Internal).map(|r| r.cash()).collect();
    assert_eq!(sweeps, [dec!(-25), dec!(25)]);
    let fee = s.records.iter().find(|r| r.kind == RecordKind::Fee).unwrap();
    assert_eq!(fee.cash(), dec!(25), "a rebate sits under credits");
    assert_eq!(fee.currency, Currency::USD);
}

#[test]
fn a_section_total_that_disagrees_fails() {
    let broken = june().replace("$500.00    $201.99", "$500.00    $201.98");
    let err = firstrade::parse_all(&[broken], &HashMap::new()).unwrap_err();
    assert!(format!("{err:#}").contains("does not match its lines"), "{err:#}");
}

#[test]
fn a_security_never_listed_needs_a_configured_ticker() {
    let err = firstrade::parse_all(&[july()], &HashMap::new()).unwrap_err();
    assert!(format!("{err:#}").contains("[symbols]"), "{err:#}");
    let names = HashMap::from([("INVENTED A CORP".to_string(), "ZZA".to_string())]);
    assert!(firstrade::parse_all(&[july()], &names).is_ok());
}
