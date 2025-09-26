use std::io::Write;

use portfolio::{cathay, Portfolio};
use tempfile::NamedTempFile;

#[test]
fn test_load_multiple_cathay_files() -> anyhow::Result<()> {
    let mut portfolio = Portfolio::new();

    // Create a temporary CSV file for 2022 data
    let csv_content_2022 = r#"根據您篩選的結果，總計有1筆資料，當前資料為1-1筆，看更多請至國泰證券app查詢
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,利息,稅款,券手續費/標借費,委託書號
元大台灣50,2022/12/26,18,"-1,997",現買,110.94,"1,996",1,0,0,0,0,0,0,p00Te"#;
    let mut file_2022 = NamedTempFile::new()?;
    file_2022.write_all(csv_content_2022.as_bytes())?;
    let path_2022 = file_2022.path().to_str().unwrap();

    // Create a temporary CSV file for 2024 data
    let csv_content_2024 = r#"根據您篩選的結果，總計有1筆資料，當前資料為1-1筆，看更多請至國泰證券app查詢
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,利息,稅款,券手續費/標借費,委託書號
國泰永續高股息,2024/01/01,100,"-2,000",現買,20.00,"2,000",0,0,0,0,0,0,0,p00XX"#;
    let mut file_2024 = NamedTempFile::new()?;
    file_2024.write_all(csv_content_2024.as_bytes())?;
    let path_2024 = file_2024.path().to_str().unwrap();

    // Load transactions from the temporary CSV files.
    cathay::load_from_cathay_csv(&mut portfolio, path_2022)?;
    cathay::load_from_cathay_csv(&mut portfolio, path_2024)?;

    let total_transactions: usize = portfolio.transactions.values().map(|v| v.len()).sum();

    // We added 1 transaction from 2022 and 1 from 2024
    assert_eq!(total_transactions, 2);

    // Optional: Check specific transaction details
    let first_transaction = portfolio.transactions.values().next().unwrap().first().unwrap();
    assert_eq!(first_transaction.symbol, "0050.TW");
    assert_eq!(first_transaction.quantity, rust_decimal::Decimal::from(18));

    Ok(())
}
