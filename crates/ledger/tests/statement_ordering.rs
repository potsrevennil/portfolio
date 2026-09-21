//! Statements are ordered by date, not by the order they are passed on the
//! command line. Two statements for the
//! same account passed either way must produce the identical ledger.

use portfolio::ledger::{self, args::Args as BuildArgs};
use tempfile::TempDir;

const MAPPING: &str = r#"
[institution]
app_account            = "國泰"
primary                = "Assets:Cathay:Savings"
settlement             = "Assets:Cathay:Investment"
settlement_app_account = "券商"
clearing               = "Assets:Cathay:Clearing"
[institution.accounts]
"111111111111" = "Assets:Cathay:Savings"
[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"
"#;

/// Older period, opening balance 0.
const EARLIER: &str = "\
查詢期間,(自 2023/01/01 至 2023/12/31)
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2023/06/01,2023/06/01,存入,,1000,1000,,
";

/// Later period, opening balance 1000.
const LATER: &str = "\
查詢期間,(自 2024/01/01 至 2024/12/31)
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2024/06/01,2024/06/01,存入,,2000,3000,,
";

#[test]
fn statement_argument_order_does_not_change_the_ledger() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let root = dir.path();
    std::fs::write(root.join("mapping.toml"), MAPPING)?;
    let earlier = root.join("earlier.csv");
    let later = root.join("later.csv");
    std::fs::write(&earlier, EARLIER)?;
    std::fs::write(&later, LATER)?;

    let generate = |order: Vec<std::path::PathBuf>| -> anyhow::Result<String> {
        let args = BuildArgs {
            cathay_statements: order,
            daily_income_expense: None,
            daily_transfers: None,
            transactions: None,
            backfill: false,
            ledger_dir: root.to_path_buf(),
        };
        ledger::build(&args)?;
        Ok(std::fs::read_to_string(root.join("generated/cathay.beancount"))?)
    };

    // Later-first on the command line used to drive the opening balance and the
    // emission order off argument order; both must now follow the dates.
    let later_first = generate(vec![later.clone(), earlier.clone()])?;
    let earlier_first = generate(vec![earlier, later])?;
    assert_eq!(later_first, earlier_first, "statement order leaked into the ledger");

    // And the result is genuinely in date order: the 2023 line precedes 2024.
    let y2023 = later_first.find("2023-06-01").expect("2023 transaction present");
    let y2024 = later_first.find("2024-06-01").expect("2024 transaction present");
    assert!(y2023 < y2024, "statements are not ordered by date");
    Ok(())
}
