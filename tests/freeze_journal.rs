//! End-to-end test for the freeze → journal → load pipeline.
//!
//! Drives the real importer over a small synthetic ledger, freezes it to a
//! journal CSV (verifying reconciliation against the statement/app balances),
//! then loads that journal into SQLite and checks the rows. Everything is
//! invented and non-sensitive; inputs are written to a temp dir at runtime so
//! no gitignored CSV is committed.

use portfolio::{
    db,
    ledger::{args::Args as BuildArgs, freeze, load, model},
};
use sqlx::Row;
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

[expenses]
"飲食" = "Expenses:Food"

[income]
"投資" = "Income:Investment"

[accounts]
"現金" = "Assets:Cash"
"美金" = "Assets:USD-Wallet"
"國泰" = "Assets:Cathay"
"券商" = "Assets:Securities:ETF"
"#;

const TRANSACTIONS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                            counter_currency,category,major_category,member,tags,note,\
                            source_party,source_file,source_id,origin,correction_note,updated_at
o:1,active,2022-01-01,,opening,1000,TWD,現金,,,,,,,,,config,m,,added,,
a:1,active,2022-02-01,,expense,200,TWD,國泰,,,,飲食,,自己,,,app,f,uuid-food-2022,raw,,
a:2,active,2022-04-01,,income,500,TWD,現金,,,,投資,,自己,,,app,f,uuid-cash-income,raw,,
a:3,active,2022-05-01,,transfer,3000,TWD,國泰,券商,3000,TWD,,,,,,app,f,uuid-etf,raw,,
a:4,active,2023-01-01,,transfer,300,TWD,現金,美金,10,USD,,,,,,app,f,uuid-fx,raw,,
a:5,active,2024-06-01,,expense,100,TWD,國泰,,,,飲食,,自己,,,app,f,uuid-food-2024,raw,,
";

const SAVINGS_STATEMENT: &str = "\
查詢期間,(自 2024/01/01 至 2024/12/31)
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2024/06/01,2024/06/01,午餐,100,,4900,,
2024/03/01,2024/03/01,薪水,,5000,5000,,
";

/// A hand-entered entry in a private account the app never saw, so it adds no
/// assertion conflict — exactly what `manual.csv` is for.
const MANUAL: &str = "\
date,account,contra,amount,currency,payee,narration,tags
2024-07-01,Assets:Petty-Cash,Income:Gifts,600,TWD,紅包,cash gift,
";

/// Writes the synthetic ledger and returns args for both stages, sharing one
/// journal path and one database.
fn fixture() -> (TempDir, freeze::FreezeArgs, load::Args) {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path();
    let write = |name: &str, contents: &str| {
        std::fs::write(root.join(name), contents).unwrap_or_else(|e| panic!("write {name}: {e}"));
    };
    write("mapping.toml", MAPPING);
    write("transactions.csv", TRANSACTIONS);
    write("savings.csv", SAVINGS_STATEMENT);
    write("manual.csv", MANUAL);

    let build = BuildArgs {
        cathay_statements: vec![root.join("savings.csv")],
        daily_income_expense: None,
        daily_transfers: None,
        transactions: Some(root.join("transactions.csv")),
        backfill: true,
        ledger_dir: root.to_path_buf(),
    };
    let freeze = freeze::FreezeArgs { journal: root.join("journal.csv"), build };
    let load = load::Args {
        journal: root.join("journal.csv"),
        database_url: format!("sqlite:{}", root.join("ledger-app.db").display()),
    };
    (dir, freeze, load)
}

#[tokio::test]
async fn freeze_then_load_reproduces_the_reconciled_history() -> anyhow::Result<()> {
    let (_dir, freeze_args, load_args) = fixture();

    // --- freeze: reconcile and export a verified journal ---
    let frozen = freeze::run(&freeze_args)?;
    assert!(frozen.ok(), "reconciliation failed:\n{frozen}");
    assert!(frozen.negatives.is_empty(), "an asset closed negative:\n{frozen}");
    assert!(frozen.assertions_checked > 0, "no assertions were checked");
    assert!(frozen.placeholders >= 1, "ETF placeholder not tagged:\n{frozen}");
    assert_eq!(frozen.conversions, 2, "cross-currency transfer not plugged:\n{frozen}");
    assert_eq!(frozen.manual, 1, "manual.csv entry not counted:\n{frozen}");
    assert!(freeze_args.journal.exists(), "the verified journal was not written");

    // --- load: the journal into SQLite ---
    let loaded = load::run(&load_args).await?;
    assert_eq!(loaded.transactions, frozen.transactions, "load lost transactions");
    assert!(loaded.postings > 0);

    let pool = db::init_db(&load_args.database_url).await?;

    // Sources cover the pipelines and are all from the allowed set.
    let sources: Vec<String> =
        sqlx::query("SELECT DISTINCT source FROM transactions ORDER BY source")
            .fetch_all(&pool)
            .await?
            .iter()
            .map(|r| r.get::<String, _>(0))
            .collect();
    assert!(sources.contains(&"import".to_string()), "no import rows: {sources:?}");
    assert!(sources.contains(&"manual".to_string()), "manual.csv not loaded: {sources:?}");

    // external_ref survived the journal round-trip: statement lines carry their id.
    let import_ref: String = sqlx::query(
        "SELECT external_ref FROM transactions WHERE source = 'import' AND external_ref IS NOT \
         NULL LIMIT 1",
    )
    .fetch_one(&pool)
    .await?
    .get(0);
    assert!(import_ref.contains("111111111111"), "statement external_ref lost: {import_ref}");

    // The declared opening balance is a transaction against the equity plug.
    let cash_opening: Vec<(String, String)> = sqlx::query(
        "SELECT a.path, p.amount FROM transactions t JOIN postings p ON p.transaction_id = t.id \
         JOIN accounts a ON a.id = p.account_id WHERE t.external_ref = 'opening:Assets:Cash:TWD' \
         ORDER BY a.path",
    )
    .fetch_all(&pool)
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect();
    assert_eq!(cash_opening, [
        ("Assets:Cash".to_string(), "1000".to_string()),
        (model::OPENING_EQUITY.to_string(), "-1000".to_string()),
    ]);

    // The placeholder account carries a retirement note (derived from the journal's
    // posting tag).
    let note: Option<String> = sqlx::query(
        "SELECT e.note FROM account_events e JOIN accounts a ON a.id = e.account_id WHERE a.path \
         = 'Assets:Securities:ETF' AND e.event = 'created'",
    )
    .fetch_one(&pool)
    .await?
    .get(0);
    assert!(note.unwrap_or_default().contains("backfilled"), "placeholder note missing");

    // Every account has exactly one 'created' event.
    let accounts: i64 = sqlx::query("SELECT COUNT(*) FROM accounts").fetch_one(&pool).await?.get(0);
    let created: i64 = sqlx::query("SELECT COUNT(*) FROM account_events WHERE event = 'created'")
        .fetch_one(&pool)
        .await?
        .get(0);
    assert_eq!(accounts, created, "not every account has a created event");
    Ok(())
}

#[tokio::test]
async fn the_freeze_is_re_runnable() -> anyhow::Result<()> {
    // Corrections are still being audited, so the freeze is expected to be re-run
    // and overwrite the journal each time.
    let (_dir, freeze_args, _load) = fixture();
    freeze::run(&freeze_args)?;
    let second = freeze::run(&freeze_args)?;
    assert!(second.ok(), "a re-run must still reconcile:\n{second}");
    Ok(())
}

#[tokio::test]
async fn a_negative_asset_fails_the_freeze_and_removes_the_journal() -> anyhow::Result<()> {
    let (_dir, freeze_args, _load) = fixture();
    // Overdraw a fresh asset account the app never funded.
    std::fs::write(
        freeze_args.build.ledger_dir.join("manual.csv"),
        "date,account,contra,amount,payee,narration\n2024-08-01,Assets:Empty-Wallet,Expenses:Food,\
         -600,overdraw,no funds\n",
    )?;

    let frozen = freeze::run(&freeze_args)?;
    assert!(!frozen.ok(), "a negative asset must fail the freeze:\n{frozen}");
    assert!(
        frozen.negatives.iter().any(|n| n.account == "Assets:Empty-Wallet"),
        "the overdrawn account was not reported:\n{frozen}"
    );
    assert!(!freeze_args.journal.exists(), "an untrusted journal must be removed");
    Ok(())
}

/// A bank 錯誤更正 reversal must collapse with the debit it undoes into ONE
/// zero-sum transaction, not fall to Expenses/Income:Uncategorized. Balance
/// assertions cannot catch a dropped netting (savings nets to zero either way),
/// so this is the only guard — it checks the netting survives freeze → journal
/// → load.
#[tokio::test]
async fn a_bank_reversal_nets_into_one_transaction_not_uncategorised() -> anyhow::Result<()> {
    const REVERSAL_MAPPING: &str = r#"
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
    // Newest-first (load reverses it): the 錯誤更正 (negative withdrawal) undoes
    // the same-day, same-counterparty 電子轉出 debit above it.
    const REVERSAL_STATEMENT: &str = "\
查詢期間,(自 2024/01/01 至 2024/12/31)
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2024/05/01,2024/05/01,錯誤更正,-500,,1000,(822)0000000000000009,
2024/05/01,2024/05/01,電子轉出,500,,500,(822)0000000000000009,
";

    let dir = TempDir::new()?;
    let root = dir.path();
    std::fs::write(root.join("mapping.toml"), REVERSAL_MAPPING)?;
    std::fs::write(root.join("savings.csv"), REVERSAL_STATEMENT)?;
    let freeze_args = freeze::FreezeArgs {
        journal: root.join("journal.csv"),
        build: BuildArgs {
            cathay_statements: vec![root.join("savings.csv")],
            daily_income_expense: None,
            daily_transfers: None,
            transactions: None,
            backfill: false,
            ledger_dir: root.to_path_buf(),
        },
    };
    let load_args = load::Args {
        journal: root.join("journal.csv"),
        database_url: format!("sqlite:{}", root.join("ledger-app.db").display()),
    };

    let frozen = freeze::run(&freeze_args)?;
    assert!(frozen.ok(), "reversal fixture did not reconcile:\n{frozen}");
    load::run(&load_args).await?;
    let pool = db::init_db(&load_args.database_url).await?;

    // The two legs collapsed into one transaction, both on the bank account and
    // summing to zero — the journal still shows the attempt.
    let reversal_legs: Vec<String> = sqlx::query(
        "SELECT p.amount FROM postings p JOIN accounts a ON a.id = p.account_id JOIN transactions \
         t ON t.id = p.transaction_id WHERE t.payee = '錯誤更正' AND a.path = \
         'Assets:Cathay:Savings' ORDER BY p.amount",
    )
    .fetch_all(&pool)
    .await?
    .iter()
    .map(|r| r.get::<String, _>(0))
    .collect();
    assert_eq!(reversal_legs, vec!["-500".to_string(), "500".to_string()], "reversal did not net");

    // The netting must NOT have inflated the uncategorised buckets — which a
    // dropped netting would, while balances still net to zero.
    let uncategorised: i64 = sqlx::query(
        "SELECT COUNT(*) FROM accounts WHERE path IN ('Expenses:Uncategorized', \
         'Income:Uncategorized')",
    )
    .fetch_one(&pool)
    .await?
    .get(0);
    assert_eq!(uncategorised, 0, "the reversal leaked into an uncategorised bucket");
    Ok(())
}

/// With the backfill, the opening it derives is what an opening row for the
/// bank account must agree with, not the statement's own later figure.
#[test]
fn an_opening_row_is_checked_against_the_backfill_opening() -> anyhow::Result<()> {
    // 國泰 maps to the savings account itself, so it can name it in a row.
    let mapping =
        MAPPING.replace(r#""國泰" = "Assets:Cathay""#, r#""國泰" = "Assets:Cathay:Savings""#);
    // Statement opens at 0 on 2024-03-01; backfill before it spends 200 on
    // 2022-02-01 and moves 3000 on 2022-05-01, so it derives an opening of 3200.
    for (amount, ok) in [("3200", true), ("0", false)] {
        let (_dir, args, _load) = fixture();
        std::fs::write(args.build.ledger_dir.join("mapping.toml"), &mapping)?;
        let records = TRANSACTIONS.replace(
            "o:1,active,2022-01-01,,opening,1000,TWD,現金",
            &format!(
                "o:1,active,2022-01-01,,opening,1000,TWD,現金,,,,,,,,,config,m,,added,,\no:2,\
                 active,2022-01-01,,opening,{amount},TWD,國泰"
            ),
        );
        std::fs::write(args.build.ledger_dir.join("transactions.csv"), records)?;
        let result = freeze::run(&args);
        match ok {
            true => assert!(result?.ok(), "a correct opening row was rejected"),
            false => {
                let err = result.expect_err("a contradicting opening row must fail");
                assert!(err.to_string().contains("contradicts the backfill"), "{err:#}");
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_journal_load_refuses_a_non_empty_database() -> anyhow::Result<()> {
    let (_dir, freeze_args, load_args) = fixture();
    freeze::run(&freeze_args)?;
    load::run(&load_args).await?;

    let err = load::run(&load_args).await.expect_err("re-loading should be refused");
    assert!(format!("{err:#}").contains("one-time"), "wrong error: {err:#}");
    Ok(())
}
