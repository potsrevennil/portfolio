//! Freeze over per-year bank exports, a 外幣 account and
//! `corrected/transactions.csv`. All data invented.

use std::collections::BTreeMap;

use portfolio::{
    currency::Currency,
    ledger::{args::Args as BuildArgs, freeze, journal},
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
"222222222222" = "Assets:Cathay:FX"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[split_accounts]
roots = ["Assets:Split"]

[expenses]
"飲食" = "Expenses:Food"

[accounts]
"國泰"     = "Assets:Cathay"
"外幣帳戶" = "Assets:Cathay:FX"
"美金"     = "Assets:USD-Wallet"
"現金"     = "Assets:Cash"
"阿明"     = "Assets:Split:Ming"

[display]
"Assets:Cathay:FX" = "外幣"
"#;

/// Entirely before the records begin: folded into the opening balance.
const SAVINGS_2023: &str = "\
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2023/03/01,2023/03/01,存入,,5000,5000,,
";

/// The TWD half of a conversion names the foreign account only in 備註.
const SAVINGS_2024: &str = "\
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2024/06/10,2024/06/10,消費,100,,1700,,
2024/06/07,2024/06/07,網銀外存,3200,,1800,,222222222222
";

const FX_2024_USD: &str = "\
\"222222222222 活存外幣\"
\"幣別：USD\"
\"交易日期\",\"帳務日期\",\"提出\",\"存入\",\"餘額\",\"成交匯率\",\"交易資訊\"
\"2024/06/11\",\"2024/06/11\",\"USD 100.00\",\"−\",\"USD 0.50\",\"−\",\"網銀轉\"
\"2024/06/07\",\"2024/06/07\",\"−\",\"USD 100.00\",\"USD 100.50\",\"32.00\",\"台幣存 \
                           111111111111TWD\"
";

const TRANSACTIONS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                            counter_currency,category,major_category,member,tags,note,\
                            source_party,source_file,source_id,origin,correction_note,updated_at
open:1,active,2023-01-01,,opening,0.50,USD,外幣帳戶,,,,,,,,,config,m,,added,,
open:2,active,2024-01-01,,opening,1000,TWD,現金,,,,,,,,,config,m,,added,,
app:1,active,2024-01-15,,transfer,50,TWD,阿明,現金,50,TWD,,,,,還錢,app,x,U1,raw,,
app:2,active,2024-06-07,,transfer,3200,TWD,國泰,外幣帳戶,100,USD,,,,,換匯,app,x,U2,raw,,
app:3,active,2024-06-10,2024-06-12,expense,100,TWD,國泰,,,,飲食,,自己,,午餐,app,x,U3,raw,,
app:4,removed,2024-06-10,,expense,999,TWD,國泰,,,,飲食,,自己,,重複,app,x,U4,removed,dup,
app:5,active,2024-06-11,,transfer,100,USD,外幣帳戶,美金,100,USD,,,,,,app,x,U5,raw,,
";

fn balances(journal: &journal::Journal) -> BTreeMap<(String, Currency), Decimal> {
    let mut out: BTreeMap<(String, Currency), Decimal> = BTreeMap::new();
    let openings = journal.openings.iter().map(|o| (&o.account, o.currency, o.amount));
    let postings = journal.postings.iter().map(|p| (&p.account, p.currency, p.amount));
    for (account, currency, amount) in openings.chain(postings) {
        *out.entry((account.clone(), currency)).or_default() += amount;
    }
    out
}

fn args(root: &std::path::Path, transactions: &str) -> anyhow::Result<freeze::FreezeArgs> {
    let write = |name: &str, contents: &str| -> anyhow::Result<std::path::PathBuf> {
        let path = root.join(name);
        std::fs::write(&path, contents)?;
        Ok(path)
    };
    write("mapping.toml", MAPPING)?;
    Ok(freeze::FreezeArgs {
        journal: root.join("journal.csv"),
        build: BuildArgs {
            cathay_statements: vec![
                write("fx-2024-USD.csv", FX_2024_USD)?,
                write("savings-2024.csv", SAVINGS_2024)?,
                write("savings-2023.csv", SAVINGS_2023)?,
            ],
            daily_income_expense: None,
            daily_transfers: None,
            transactions: Some(write("transactions.csv", transactions)?),
            backfill: true,
            ledger_dir: root.to_path_buf(),
        },
    })
}

/// An opening that contradicts its statement (amount, or a later date) fails.
#[test]
fn a_declared_opening_contradicting_its_statement_fails() -> anyhow::Result<()> {
    let declared = "2023-01-01,,opening,0.50,USD";
    for contradiction in ["2023-01-01,,opening,0.75,USD", "2024-06-08,,opening,0.50,USD"] {
        let dir = TempDir::new()?;
        let args = args(dir.path(), &TRANSACTIONS.replace(declared, contradiction))?;
        let err = freeze::run(&args).expect_err("a contradicting opening must fail");
        assert!(err.to_string().contains("contradicts its statement"), "{err:#}");
    }
    Ok(())
}

#[test]
fn freeze_reads_yearly_exports_a_foreign_account_and_corrected_records() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = args(dir.path(), TRANSACTIONS)?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    assert_eq!(frozen.conversions, 2, "the conversion is not plugged per currency:\n{frozen}");

    let written = journal::read(&args.journal)?;
    let bal = balances(&written);
    let get = |account: &str, currency: Currency| {
        bal.get(&(account.to_string(), currency)).copied().unwrap_or_default()
    };
    assert_eq!(get("Assets:Cathay:Savings", Currency::TWD), dec!(1700));
    assert_eq!(get("Assets:Cathay:FX", Currency::USD), dec!(0.50), "declared opening doubled");
    assert_eq!(get("Assets:Cathay:FX", Currency::TWD), dec!(0), "FX booked in TWD");
    assert_eq!(get("Assets:USD-Wallet", Currency::USD), dec!(100));
    assert_eq!(get("Assets:Split:Ming", Currency::TWD), dec!(-50), "split account may be negative");
    assert_eq!(get("Expenses:Food", Currency::TWD), dec!(100), "a removed row was read");
    for bucket in ["Assets:Cathay:Clearing", "Expenses:Uncategorized", "Income:Uncategorized"] {
        assert!(
            !bal.keys().any(|(a, _)| a == bucket),
            "{bucket} used — a conversion half was not paired"
        );
    }

    let savings_opening = written
        .openings
        .iter()
        .find(|o| o.account == "Assets:Cathay:Savings")
        .expect("savings opening");
    assert_eq!(savings_opening.amount, dec!(5000), "the 2023 line was not folded in");

    assert!(
        written
            .postings
            .iter()
            .any(|p| p.external_ref.as_deref() == Some("222222222222:2024-06-11:-100.00:0.50")),
        "the FX line's dedup key is missing"
    );
    Ok(())
}
