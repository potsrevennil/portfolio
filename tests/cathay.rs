use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
};

use chrono::NaiveDate;
use portfolio::{
    cathay,
    portfolio::{Broker, Event, Security, TransactionKind},
    Portfolio,
};
use tempfile::NamedTempFile;

#[test]
fn test_load_multiple_cathay_files() -> anyhow::Result<()> {
    let mut aggregated_events: BTreeMap<NaiveDate, Event> = BTreeMap::new();
    let mut aggregated_securities: HashMap<String, Security> = HashMap::new();

    // Create temporary CSV files
    let csv_content_2022 = r#"根據您篩選的結果，總計有1筆資料，當前資料為1-1筆，看更多請至國泰證券app查詢
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,利息,稅款,券手續費/標借費,委託書號
範例證券01,2022/03/02,10,"-1,001",現買,100.00,"1,000",1,0,0,0,0,0,0,A0001"#;
    let mut file_2022 = NamedTempFile::new()?;
    file_2022.write_all(csv_content_2022.as_bytes())?;
    let path_2022 = file_2022.path().to_str().unwrap();

    let csv_content_2024 = r#"根據您篩選的結果，總計有1筆資料，當前資料為1-1筆，看更多請至國泰證券app查詢
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,利息,稅款,券手續費/標借費,委託書號
範例證券08,2024/01/01,100,"-2,000",現買,20.00,"2,000",0,0,0,0,0,0,0,p00XX"#;
    let mut file_2024 = NamedTempFile::new()?;
    file_2024.write_all(csv_content_2024.as_bytes())?;
    let path_2024 = file_2024.path().to_str().unwrap();

    let csv_content_2025 = r#"根據您篩選的結果，總計有1筆資料，當前資料為1-1筆，看更多請至國泰證券app查詢
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,利息,稅款,券手續費/標借費,委託書號
範例證券11,2025/03/03,"1,000","49,900",現賣,50.00,"50,000",20,80,0,0,0,0,0,A0002"#;
    let mut file_2025 = NamedTempFile::new()?;
    file_2025.write_all(csv_content_2025.as_bytes())?;
    let path_2025 = file_2025.path().to_str().unwrap();

    // Load transactions from the temporary CSV files and aggregate
    let (events_2022, securities_2022) = cathay::load_from_csv(path_2022)?;
    aggregated_events.extend(events_2022);
    aggregated_securities.extend(securities_2022);

    let (events_2024, securities_2024) = cathay::load_from_csv(path_2024)?;
    aggregated_events.extend(events_2024);
    aggregated_securities.extend(securities_2024);

    let (events_2025, securities_2025) = cathay::load_from_csv(path_2025)?;
    aggregated_events.extend(events_2025);
    aggregated_securities.extend(securities_2025);

    let portfolio = Portfolio::new(Broker::Cathay, aggregated_events, aggregated_securities);

    let mut total_transactions = 0;
    let mut deposit_count = 0;
    let mut withdrawal_count = 0;
    let mut buy_count = 0;
    let mut sell_count = 0;

    for (_, event) in portfolio.events.iter() {
        for transaction in &event.transactions {
            total_transactions += 1;
            match transaction.kind {
                TransactionKind::Deposit => {
                    deposit_count += 1;
                    // Assert the amount for the deposit from 2022 buy
                    if transaction.datetime.date_naive().to_string() == "2022-03-02" {
                        assert_eq!(transaction.amount, rust_decimal::Decimal::from(1001));
                    }
                    // Assert the amount for the deposit from 2024 buy
                    if transaction.datetime.date_naive().to_string() == "2024-01-01" {
                        assert_eq!(transaction.amount, rust_decimal::Decimal::from(2000));
                    }
                }
                TransactionKind::Withdrawal => {
                    withdrawal_count += 1;
                    // Assert the amount for the withdrawal from 2025 sell
                    if transaction.datetime.date_naive().to_string() == "2025-03-03" {
                        assert_eq!(transaction.amount, rust_decimal::Decimal::from(-49900));
                    }
                }
                TransactionKind::Buy => buy_count += 1,
                TransactionKind::Sell => sell_count += 1,
                _ => {}
            }
        }
    }

    // We added 2 buy transactions and 1 sell transaction.
    // Each should generate an implicit cash flow transaction.
    // So, 3 original trades + 3 cash flow events = 6 total transactions.
    assert_eq!(total_transactions, 6);
    assert_eq!(buy_count, 2);
    assert_eq!(sell_count, 1);
    assert_eq!(deposit_count, 2);
    assert_eq!(withdrawal_count, 1);

    // Optional: Check specific transaction details (original buy from 2022)
    let first_transaction_2022 = portfolio
        .events
        .get(&chrono::NaiveDate::from_ymd_opt(2022, 3, 2).unwrap())
        .unwrap()
        .transactions
        .iter()
        .find(|t| t.kind == TransactionKind::Buy)
        .unwrap();
    assert_eq!(first_transaction_2022.symbol, "ZZ01.TW");
    assert_eq!(first_transaction_2022.quantity, rust_decimal::Decimal::from(10));

    Ok(())
}
