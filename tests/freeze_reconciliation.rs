//! How the freeze reconciles statement lines against app records: internal
//! transfers, conversions, records no line explains, and a statement that does
//! not add up. All data invented.

use std::collections::{BTreeMap, BTreeSet};

use portfolio::{
    currency::Currency,
    ledger::{args::Args as BuildArgs, build, freeze, journal, load},
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
"333333333333" = "Assets:Cathay:Investment"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[fallback.descriptions]
"利息" = "Income:Interest"

[income]
"薪資" = "Income:Salary"

[expenses]
"飲食" = "Expenses:Food"

[accounts]
"國泰"     = "Assets:Cathay"
"外幣帳戶" = "Assets:Cathay:FX"
"現金"     = "Assets:Cash"
"美金"     = "Assets:USD-Wallet"
"卡"       = "Liabilities:Card"
"起鼓"     = "Equity:Opening-Balances"

[overrides]
"U-GIFT" = { account = "Expenses:Gifts", narration = "禮物" }
"U-CASH" = { account = "Expenses:Gifts", narration = "不應套用" }
"#;

const HEADER: &str = "交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n";

const RECORDS_HEADER: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                              counter_currency,category,major_category,member,tags,note,\
                              source_party,source_file,source_id,origin,correction_note,\
                              updated_at\n";

/// A TWD statement for `account_no`, rows newest-first as the bank exports
/// them.
fn statement(account_no: &str, rows: &str) -> String {
    format!("{account_no} 活存\n幣別：TWD\n{HEADER}{rows}")
}

/// Where the inputs of one freeze live.
enum Records<'a> {
    None,
    Corrected(&'a str),
    Exports { income_expense: &'a str, transfers: &'a str },
}

/// Writes the inputs and returns the freeze args; the directory must outlive
/// them.
fn inputs(
    dir: &TempDir,
    statements: &[(&str, &str)],
    records: Records,
) -> anyhow::Result<freeze::FreezeArgs> {
    let root = dir.path();
    let write = |name: &str, contents: &str| -> anyhow::Result<std::path::PathBuf> {
        let path = root.join(name);
        std::fs::write(&path, contents)?;
        Ok(path)
    };
    write("mapping.toml", MAPPING)?;
    let cathay_statements =
        statements.iter().map(|(name, body)| write(name, body)).collect::<anyhow::Result<_>>()?;
    let (transactions, daily_income_expense, daily_transfers) = match records {
        Records::None => (None, None, None),
        Records::Corrected(rows) => {
            (Some(write("transactions.csv", &format!("{RECORDS_HEADER}{rows}"))?), None, None)
        }
        Records::Exports { income_expense, transfers } => (
            None,
            Some(write("income_expense.csv", income_expense)?),
            Some(write("transfers.csv", transfers)?),
        ),
    };
    Ok(freeze::FreezeArgs {
        journal: root.join("journal.csv"),
        build: BuildArgs {
            cathay_statements,
            daily_income_expense,
            daily_transfers,
            transactions,
            backfill: false,
            ledger_dir: root.to_path_buf(),
        },
    })
}

fn balances(journal: &journal::Journal) -> BTreeMap<(String, Currency), Decimal> {
    let mut out: BTreeMap<(String, Currency), Decimal> = BTreeMap::new();
    for p in &journal.postings {
        *out.entry((p.account.clone(), p.currency)).or_default() += p.amount;
    }
    out
}

fn balance(journal: &journal::Journal, account: &str, currency: Currency) -> Decimal {
    balances(journal).get(&(account.to_string(), currency)).copied().unwrap_or_default()
}

/// Each transaction's legs as (account, amount, currency), keyed by group.
fn groups(journal: &journal::Journal) -> BTreeMap<u64, Vec<(String, Decimal, Currency)>> {
    let mut out: BTreeMap<u64, Vec<(String, Decimal, Currency)>> = BTreeMap::new();
    for p in &journal.postings {
        out.entry(p.group).or_default().push((p.account.clone(), p.amount, p.currency));
    }
    out
}

/// Both halves of a same-day move between two of your own statements become
/// one transaction; nothing is left on the clearing account.
#[test]
fn a_same_day_internal_transfer_is_one_transaction() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[
            (
                "savings.csv",
                &statement(
                    "111111111111",
                    "2024/06/08,2024/06/08,手續費,15,,3985,,\n2024/06/05,2024/06/05,轉出,1000,,\
                     4000,333333333333,\n2024/06/01,2024/06/01,利息,,5000,5000,,\n",
                ),
            ),
            (
                "investment.csv",
                &statement("333333333333", "2024/06/05,2024/06/05,轉入,,1000,1000,111111111111,\n"),
            ),
        ],
        Records::None,
    )?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    let written = journal::read(&args.journal)?;

    let transfer: Vec<_> = groups(&written)
        .into_values()
        .filter(|legs| {
            legs.iter().any(|(a, amount, _)| a == "Assets:Cathay:Investment" && !amount.is_zero())
        })
        .collect();
    assert_eq!(
        transfer,
        vec![vec![
            ("Assets:Cathay:Savings".to_string(), dec!(-1000), Currency::TWD),
            ("Assets:Cathay:Investment".to_string(), dec!(1000), Currency::TWD),
        ]],
        "the two halves were not paired into one transaction"
    );
    assert!(!written.postings.iter().any(|p| p.account == "Assets:Cathay:Clearing"));
    // Lines no record explains fall back: by description when it is known, else by
    // direction.
    assert_eq!(balance(&written, "Income:Interest", Currency::TWD), dec!(-5000));
    assert_eq!(balance(&written, "Expenses:Uncategorized", Currency::TWD), dec!(15));
    Ok(())
}

/// Halves landing on different days were genuinely in transit, so each goes
/// through the clearing account, which nets back to zero.
#[test]
fn an_internal_transfer_spanning_two_days_goes_through_clearing() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[
            (
                "savings.csv",
                &statement(
                    "111111111111",
                    "2024/06/05,2024/06/05,轉出,1000,,4000,333333333333,\n2024/06/01,2024/06/01,\
                     存入,,5000,5000,,\n",
                ),
            ),
            (
                "investment.csv",
                &statement("333333333333", "2024/06/06,2024/06/06,轉入,,1000,1000,111111111111,\n"),
            ),
        ],
        Records::None,
    )?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    let written = journal::read(&args.journal)?;

    let clearing: Vec<Decimal> = written
        .postings
        .iter()
        .filter(|p| p.account == "Assets:Cathay:Clearing")
        .map(|p| p.amount)
        .collect();
    assert_eq!(clearing, vec![dec!(1000), dec!(-1000)], "each half should post to clearing");
    assert_eq!(balance(&written, "Assets:Cathay:Investment", Currency::TWD), dec!(1000));
    Ok(())
}

/// With no app record to tell two currencies apart, a conversion still pairs
/// when there is exactly one candidate for it.
#[test]
fn a_lone_conversion_pairs_without_an_app_record() -> anyhow::Result<()> {
    const FX: &str = "\
222222222222 活存外幣
幣別：USD
交易日期,帳務日期,提出,存入,餘額,成交匯率,交易資訊
2024/06/07,2024/06/07,−,USD 100.00,USD 100.00,32,台幣存 111111111111TWD
";
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[
            (
                "savings.csv",
                &statement(
                    "111111111111",
                    "2024/06/07,2024/06/07,網銀外存,3200,,1800,,222222222222\n2024/06/01,2024/06/\
                     01,存入,,5000,5000,,\n",
                ),
            ),
            ("fx.csv", FX),
        ],
        Records::None,
    )?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    assert_eq!(frozen.conversions, 2, "one conversion leg per currency:\n{frozen}");
    let written = journal::read(&args.journal)?;
    assert!(
        groups(&written).values().any(|legs| {
            legs.contains(&("Assets:Cathay:Savings".to_string(), dec!(-3200), Currency::TWD))
                && legs.contains(&("Assets:Cathay:FX".to_string(), dec!(100.00), Currency::USD))
        }),
        "the conversion halves were not paired"
    );
    assert!(!written.postings.iter().any(|p| p.account == "Expenses:Uncategorized"));
    Ok(())
}

/// A record touching the bank that no statement line explains keeps its far
/// side; the near side goes to the uncategorised bucket by direction. A named
/// correction and an unmapped category apply to matched lines as well.
#[test]
fn records_no_statement_line_explains_keep_their_far_side() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[(
            "savings.csv",
            &statement(
                "111111111111",
                "2024/06/10,2024/06/10,轉帳,100,,4900,,\n2024/06/01,2024/06/01,存入,,5000,5000,,\n",
            ),
        )],
        Records::Corrected(
            "o:1,active,2024-01-01,,transfer,500,TWD,卡,起鼓,500,TWD,,,,,,config,m,,added,,
a:1,active,2024-06-01,,income,5000,TWD,國泰,,,,未知,,自己,,,app,f,U-UNMAPPED,raw,,
a:2,active,2024-06-10,,expense,100,TWD,國泰,,,,飲食,,自己,,,app,f,U-GIFT,raw,,
a:3,active,2024-06-12,,transfer,300,TWD,國泰,現金,300,TWD,,,,,,app,f,U-CASH,raw,,
a:4,active,2024-06-13,,income,40,TWD,國泰,,,,薪資,,自己,,,app,f,U-SALARY,raw,,
a:5,active,2024-06-14,,expense,0,TWD,國泰,,,,飲食,,自己,,,app,f,U-ZERO,raw,,
a:6,active,2024-06-15,,transfer,3000,TWD,國泰,美金,100,USD,,,,,,app,f,U-USD,raw,,
",
        ),
    )?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    let written = journal::read(&args.journal)?;

    // The far sides the statement never saw.
    assert_eq!(balance(&written, "Assets:Cash", Currency::TWD), dec!(300));
    assert_eq!(balance(&written, "Income:Salary", Currency::TWD), dec!(-40));
    assert_eq!(balance(&written, "Assets:USD-Wallet", Currency::USD), dec!(100));
    assert_eq!(balance(&written, "Assets:USD-Wallet", Currency::TWD), dec!(0), "booked in TWD");
    // The near sides, bucketed by direction.
    assert_eq!(balance(&written, "Expenses:Uncategorized", Currency::TWD), dec!(-3300));
    // +40 near side of the salary record, -5000 for the matched line whose
    // category is unmapped.
    assert_eq!(balance(&written, "Income:Uncategorized", Currency::TWD), dec!(-4960));
    // One transaction per record, each keyed by its app id — transfers
    // included; the zero record is not among them.
    let unmatched: BTreeSet<&str> = written
        .postings
        .iter()
        .filter(|p| p.payee.as_deref() == Some("未對應紀錄"))
        .filter_map(|p| p.external_ref.as_deref())
        .collect();
    assert_eq!(unmatched, BTreeSet::from(["U-CASH", "U-SALARY", "U-USD"]));

    // The correction beat the 飲食 category on the matched line — and only
    // there: U-CASH names a 轉帳 row, whose far side is an account already, so
    // the correction has no category to replace and must not rewrite it.
    assert_eq!(balance(&written, "Expenses:Gifts", Currency::TWD), dec!(100));
    assert_eq!(balance(&written, "Expenses:Food", Currency::TWD), dec!(0));
    assert!(
        written.postings.iter().any(|p| p.external_ref.as_deref() == Some("U-CASH")
            && p.account == "Assets:Cash"
            && p.narration.is_empty()),
        "a correction rewrote a 轉帳 row"
    );
    // An opening can be a transfer into the equity, which makes it negative.
    assert_eq!(balance(&written, "Liabilities:Card", Currency::TWD), dec!(-500));
    Ok(())
}

/// An opening naming an account the config does not map cannot be placed.
#[test]
fn an_opening_for_an_unmapped_account_fails() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[("savings.csv", &statement("111111111111", "2024/06/01,2024/06/01,存入,,5000,5000,,\n"))],
        Records::Corrected(
            "o:1,active,2024-01-01,,transfer,500,TWD,起鼓,錢包,500,TWD,,,,,,config,m,,added,,\n",
        ),
    )?;
    let err = freeze::run(&args).expect_err("an unmapped opening must fail");
    assert!(format!("{err:#}").contains("錢包"), "error should name the account: {err:#}");
    Ok(())
}

/// The two raw 天天記帳 exports work in place of corrected records: blank
/// cells, trailer rows and zero movements are skipped, and an unmapped name
/// falls to a placeholder instead of vanishing.
#[test]
fn freeze_reads_the_raw_app_exports() -> anyhow::Result<()> {
    const INCOME_EXPENSE: &str = "\
日期,類別,大類別,金額,幣別,成員,帳戶,標籤,備註,收支區分,上次更新,UUID
20240601,薪資,,5000,TWD,自己,國泰,,,收,2024-06-01 00:00:00,U1
20240610,飲食,,100,TWD,自己,國泰,,,支,2024-06-10 00:00:00,U2
20240602,飲食,,,TWD,自己,現金,,,支,2024-06-02 00:00:00,U3
20240603,飲食,,50,TWD,自己,,,,支,2024-06-03 00:00:00,U4
20240604,神秘,,20,TWD,自己,現金,,,支,2024-06-04 00:00:00,U5
20240605,薪資,,30,TWD,自己,錢包,,,收,2024-06-05 00:00:00,U6
20240607,神秘,,10,TWD,自己,現金,,,收,2024-06-07 00:00:00,U7
";
    const TRANSFERS: &str = "\
日期,從帳戶,轉出金額,幣別,到帳戶,轉入金額,幣別,標籤,備註,上次更新,UUID
20240101,起鼓,500,TWD,現金,500,TWD,,,2024-01-01 00:00:00,T1
20240606,現金,0,TWD,美金,0,USD,,,2024-06-06 00:00:00,T2
20240608,現金,60,TWD,美金,2,USD,,,2024-06-08 00:00:00,T3
合計,,500,,,500,,,,,
";
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[(
            "savings.csv",
            &statement(
                "111111111111",
                "2024/06/10,2024/06/10,午餐,100,,4900,,\n2024/06/01,2024/06/01,存入,,5000,5000,,\n",
            ),
        )],
        Records::Exports { income_expense: INCOME_EXPENSE, transfers: TRANSFERS },
    )?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    let written = journal::read(&args.journal)?;

    assert_eq!(balance(&written, "Income:Salary", Currency::TWD), dec!(-5030));
    assert_eq!(
        balance(&written, "Expenses:Food", Currency::TWD),
        dec!(100),
        "a blank row was read"
    );
    assert_eq!(balance(&written, "Assets:Cash", Currency::TWD), dec!(430));
    // An unmapped category falls to the bucket for its direction.
    assert_eq!(balance(&written, "Expenses:Uncategorized", Currency::TWD), dec!(20));
    assert_eq!(balance(&written, "Income:Uncategorized", Currency::TWD), dec!(-10));
    assert_eq!(balance(&written, "Assets:Unmapped", Currency::TWD), dec!(30));
    // A transfer between two accounts with no statement is one transaction
    // keyed by its app id; the zero one is dropped.
    let transfer: Vec<(&str, Decimal, Currency)> = written
        .postings
        .iter()
        .filter(|p| p.external_ref.as_deref() == Some("T3"))
        .map(|p| (p.account.as_str(), p.amount, p.currency))
        .collect();
    assert!(transfer.contains(&("Assets:Cash", dec!(-60), Currency::TWD)), "{transfer:?}");
    assert!(transfer.contains(&("Assets:USD-Wallet", dec!(2), Currency::USD)), "{transfer:?}");
    assert!(!written.postings.iter().any(|p| p.external_ref.as_deref() == Some("T2")));
    Ok(())
}

/// A statement whose running balance does not follow from its own lines cannot
/// be reproduced, so the freeze reports the mismatch and keeps no journal.
#[test]
fn a_statement_that_does_not_add_up_fails_the_freeze() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[(
            "savings.csv",
            &statement(
                "111111111111",
                "2024/06/10,2024/06/10,午餐,100,,4800,,\n2024/06/01,2024/06/01,存入,,5000,5000,,\n",
            ),
        )],
        Records::None,
    )?;

    let frozen = freeze::run(&args)?;
    assert!(!frozen.ok(), "a balance the lines cannot reach must fail:\n{frozen}");
    let [mismatch] = frozen.mismatches.as_slice() else {
        panic!("expected one mismatch:\n{frozen}");
    };
    assert_eq!(mismatch.account, "Assets:Cathay:Savings");
    // The last day may be the download day, so the day before is what the
    // lines cannot reach: 4900 by the statement, 5000 by its lines.
    assert_eq!((mismatch.expected, mismatch.actual), (dec!(4900), dec!(5000)));
    let shown = frozen.to_string();
    assert!(shown.contains("reconciliation FAILED"), "{shown}");
    assert!(shown.contains("MISMATCH Assets:Cathay:Savings TWD"), "{shown}");
    assert!(!args.journal.exists(), "an untrusted journal must be removed");
    Ok(())
}

/// A transfer between two accounts that each have a statement is one record
/// seen from both pools. Emitting it once per pool would write the same record
/// twice under one id, which the dedup index then refuses at load.
#[tokio::test]
async fn a_transfer_between_two_statement_accounts_is_emitted_once() -> anyhow::Result<()> {
    const FX: &str = "\
222222222222 活存外幣
幣別：USD
交易日期,帳務日期,提出,存入,餘額,成交匯率,交易資訊
2024/06/07,2024/06/07,−,USD 100.00,USD 100.00,32,台幣存 111111111111TWD
";
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[
            (
                "savings.csv",
                &statement(
                    "111111111111",
                    "2024/06/07,2024/06/07,網銀外存,3200,,1800,,222222222222\n2024/06/01,2024/06/\
                     01,存入,,5000,5000,,\n",
                ),
            ),
            ("fx.csv", FX),
        ],
        // The 外幣 → 國泰 move is dated after both statements end, so no line
        // can explain it and no assertion covers it.
        Records::Corrected(
            "a:0,active,2024-06-01,,income,5000,TWD,國泰,,,,薪資,,自己,,,app,f,U-PAY,raw,,
a:1,active,2024-07-01,,transfer,15,USD,外幣帳戶,國泰,500,TWD,,,,,,app,f,U-LATE,raw,,
",
        ),
    )?;

    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "did not reconcile:\n{frozen}");
    let written = journal::read(&args.journal)?;
    let groups: BTreeSet<u64> = written
        .postings
        .iter()
        .filter(|p| p.external_ref.as_deref() == Some("U-LATE"))
        .map(|p| p.group)
        .collect();
    assert_eq!(groups.len(), 1, "the record was emitted {} times", groups.len());

    // The dedup index is the real judge of one-record-one-transaction.
    let load_args = load::Args {
        journal: args.journal.clone(),
        database_url: format!("sqlite:{}", dir.path().join("ledger-app.db").display()),
    };
    load::run(&load_args).await?;
    Ok(())
}

/// The same record, but pre-anchor and with the backfill on: the backfill
/// emits the institution pool's side, so the other pool must not emit it again
/// under the same id. Checked on the assembled model, because this shape also
/// breaks the far account's balance assertion — which stops the freeze, but
/// only by luck, and says nothing about the duplicate.
#[test]
fn a_pre_anchor_transfer_is_not_emitted_twice() -> anyhow::Result<()> {
    const FX: &str = "\
222222222222 活存外幣
幣別：USD
交易日期,帳務日期,提出,存入,餘額,成交匯率,交易資訊
2024/06/07,2024/06/07,−,USD 100.00,USD 100.00,32,台幣存 111111111111TWD
";
    let dir = TempDir::new()?;
    let mut args = inputs(
        &dir,
        &[
            (
                "savings.csv",
                &statement(
                    "111111111111",
                    "2024/06/07,2024/06/07,網銀外存,3200,,1800,,222222222222\n2024/06/01,2024/06/\
                     01,存入,,5000,5000,,\n",
                ),
            ),
            ("fx.csv", FX),
        ],
        Records::Corrected(
            "a:0,active,2024-05-01,,transfer,15,USD,外幣帳戶,國泰,500,TWD,,,,,,app,f,U-PRE,raw,,
a:1,active,2024-06-01,,income,5000,TWD,國泰,,,,薪資,,自己,,,app,f,U-PAY,raw,,
",
        ),
    )?;
    args.build.backfill = true;

    let (model, _summary) = build::assemble(&args.build)?;
    let carrying: Vec<&str> = model
        .transactions()
        .filter(|t| t.external_ref.as_deref() == Some("U-PRE"))
        .map(|t| t.payee.as_str())
        .collect();
    assert_eq!(carrying.len(), 1, "the record was emitted {} times", carrying.len());
    Ok(())
}

/// Two records that happen to carry one id are still two movements: the
/// one-emission rule keys on which record it is, so neither far side is lost
/// and the dedup key they share is reported rather than silently merged.
#[test]
fn two_records_sharing_an_id_are_both_kept() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let args = inputs(
        &dir,
        &[("savings.csv", &statement("111111111111", "2024/06/01,2024/06/01,存入,,5000,5000,,\n"))],
        Records::Corrected(
            "a:0,active,2024-06-01,,income,5000,TWD,國泰,,,,薪資,,自己,,,app,f,U-PAY,raw,,
a:1,active,2024-06-05,,transfer,300,TWD,國泰,現金,300,TWD,,,,,,app,f,U-SAME,raw,,
a:2,active,2024-06-06,,transfer,400,TWD,國泰,現金,400,TWD,,,,,,app,f,U-SAME,raw,,
",
        ),
    )?;

    let err = freeze::run(&args).expect_err("one dedup key on two records");
    assert!(format!("{err:#}").contains("(tiantian, U-SAME)"), "got: {err:#}");
    Ok(())
}
