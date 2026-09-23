//! Cathay securities export quirks, on invented trades.

use std::collections::HashMap;

use portfolio::broker::{cathay, RecordKind};
use rust_decimal_macros::dec;

const HEADER: &str = "根據您篩選的結果，總計有2筆資料
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,\
                      利息,稅款,券手續費/標借費,委託書號
";

fn names() -> HashMap<String, String> {
    HashMap::from([("範例一".to_string(), "ZZ01.TW".to_string())])
}

/// An order filled in two executions prints one 委託書號 on both rows.
#[test]
fn a_repeated_order_number_stays_two_trades() {
    let export = format!(
        "\u{feff}{HEADER}範例一,2022/12/26,10,\"-1,001\",現買,100,\"1,000\",1,0,0,0,0,0,0,A0001
範例一,2022/12/26,5,-506,現買,101,505,1,0,0,0,0,0,0,A0001
"
    );
    let s = cathay::parse(&export, &names()).unwrap();
    assert_eq!(s.records.len(), 2);
    assert_ne!(s.records[0].key, s.records[1].key, "the repeat is numbered");
    assert_eq!(s.records[1].key, format!("{}#2", s.records[0].key));
    assert_eq!(s.records.iter().map(|r| r.quantity).sum::<rust_decimal::Decimal>(), dec!(15));
}

#[test]
fn a_sale_carries_its_fee_and_tax() {
    let export = format!(
        "\u{feff}{HEADER}範例一,2025/03/03,\"1,000\",\"49,900\",現賣,50,\"50,000\",20,80,0,0,0,0,\
         0,A0002\n"
    );
    let [sale] = cathay::parse(&export, &names()).unwrap().records.try_into().unwrap();
    assert_eq!(sale.kind, RecordKind::Sell);
    assert_eq!(
        (sale.quantity, sale.amount, sale.commission),
        (dec!(-1000), dec!(50000), dec!(-100))
    );
    assert_eq!(sale.cash(), dec!(49900), "the net the export states");
}

#[test]
fn a_row_whose_parts_dont_add_up_fails() {
    let export = format!(
        "\u{feff}{HEADER}範例一,2025/03/03,\"1,000\",\"49,000\",現賣,50,\"50,000\",20,80,0,0,0,0,\
         0,A0002\n"
    );
    let err = cathay::parse(&export, &names()).unwrap_err();
    assert!(format!("{err:#}").contains("don't add up"), "{err:#}");
}

#[test]
fn an_unknown_stock_name_names_itself() {
    let export =
        format!("\u{feff}{HEADER}範例二,2025/03/03,10,-1001,現買,100,1000,1,0,0,0,0,0,0,A0003\n");
    let err = cathay::parse(&export, &names()).unwrap_err();
    assert!(format!("{err:#}").contains("[symbols]"), "{err:#}");
}
