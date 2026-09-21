//! End-to-end test for the freeze → journal → load pipeline.
//!
//! Drives the real importer over a small synthetic ledger, freezes it to a
//! journal CSV (verifying reconciliation against the statement/app balances),
//! then loads that journal into SQLite and checks the rows. Everything is
//! invented and non-sensitive; inputs are written to a temp dir at runtime so
//! no gitignored CSV is committed.

use chrono::NaiveDate;
use portfolio::{
    currency::Currency,
    db,
    ledger::{args::Args as BuildArgs, freeze, journal, load, model},
    store::{
        self,
        assertions::{self, AssertionSource, BalanceAssertion},
    },
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
"券商" = "Assets:Broker:Holdings"
"起鼓" = "Equity:Opening-Balances"
"#;

const TRANSACTIONS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                            counter_currency,category,major_category,member,tags,note,\
                            source_party,source_file,source_id,origin,correction_note,updated_at
o:1,active,2022-01-01,,transfer,1000,TWD,起鼓,現金,1000,TWD,,,,,,config,m,,added,,
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
        mapping: root.join("mapping.toml"),
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
    assert!(frozen.figures_checked > 0, "no balance figures were checked");
    assert!(frozen.placeholders >= 1, "ETF placeholder not tagged:\n{frozen}");
    assert_eq!(frozen.conversions, 2, "cross-currency transfer not plugged:\n{frozen}");
    assert_eq!(frozen.manual, 1, "manual.csv entry not counted:\n{frozen}");
    assert!(freeze_args.journal.exists(), "the verified journal was not written");
    let shown = frozen.to_string();
    assert!(shown.contains("froze reconciled history"), "{shown}");
    assert!(shown.contains("1 manual.csv entries are in the journal"), "{shown}");

    // --- load: the journal into SQLite ---
    let loaded = load::run(&load_args).await?;
    assert_eq!(loaded.transactions, frozen.transactions, "load lost transactions");
    assert!(loaded.postings > 0);
    assert!(loaded.to_string().contains(&format!("{} transactions", loaded.transactions)));

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
         = 'Assets:Broker:Holdings' AND e.event = 'created'",
    )
    .fetch_one(&pool)
    .await?
    .get(0);
    assert!(
        note.unwrap_or_default().contains("backfilled"),
        "the placeholder subtree is read from the chart, so a securities account named anything \
         else must still be tagged"
    );

    // Every account has exactly one 'created' event.
    let accounts: i64 = sqlx::query("SELECT COUNT(*) FROM accounts").fetch_one(&pool).await?.get(0);
    let created: i64 = sqlx::query("SELECT COUNT(*) FROM account_events WHERE event = 'created'")
        .fetch_one(&pool)
        .await?
        .get(0);
    assert_eq!(accounts, created, "not every account has a created event");

    // Frozen history is reviewed by definition; none of it lands in the queue.
    let unreviewed: i64 = sqlx::query("SELECT COUNT(*) FROM transactions WHERE reviewed = 0")
        .fetch_one(&pool)
        .await?
        .get(0);
    assert_eq!(unreviewed, 0, "load-journal left history unreviewed");

    // Every assertion freeze verified is in SQLite, and the load's check passed.
    let written = journal::read_assertions(&journal::assertions_path(&freeze_args.journal))?;
    let stored = assertions::load(&mut *pool.acquire().await?).await?;
    assert_eq!(stored.len(), written.len());
    let statement = stored
        .iter()
        .find(|a| a.source == AssertionSource::Statement)
        .expect("the savings statement period was not loaded");
    assert_eq!(
        (statement.account.as_str(), statement.opening, statement.closing),
        ("Assets:Cathay:Savings", Some(dec!(0)), dec!(4900))
    );
    assert_eq!(statement.period_start, NaiveDate::from_ymd_opt(2024, 3, 1));
    assert!(stored.iter().any(|a| a.source == AssertionSource::Tiantian));
    let figures: usize = stored.iter().map(|a| a.points().count()).sum();
    assert!(loaded.check.ok() && loaded.check.figures() == figures, "{}", loaded.check);

    // Labels come from mapping.toml; ancestors are accounts too, so every tree
    // level has one.
    let labels: Vec<(String, String)> = sqlx::query(
        "SELECT path, label FROM accounts WHERE path IN ('Assets', 'Assets:Cash', \
         'Assets:Broker') ORDER BY path",
    )
    .fetch_all(&pool)
    .await?
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect();
    assert_eq!(labels, [
        ("Assets".to_string(), "Assets".to_string()),
        ("Assets:Broker".to_string(), "Broker".to_string()),
        ("Assets:Cash".to_string(), "現金".to_string()),
    ]);
    Ok(())
}

/// A journal that disagrees with its statement must not load: the gate runs
/// inside the load's transaction, so nothing commits.
#[tokio::test]
async fn load_journal_refuses_history_that_fails_the_check() -> anyhow::Result<()> {
    let (_dir, freeze_args, load_args) = fixture();
    freeze::run(&freeze_args)?;
    let path = journal::assertions_path(&freeze_args.journal);
    let mut tampered = journal::read_assertions(&path)?;
    let statement = tampered
        .iter_mut()
        .find(|a| a.source == AssertionSource::Statement)
        .expect("a statement assertion");
    statement.closing += dec!(1);
    journal::write_assertions(&path, &tampered)?;

    let err = load::run(&load_args).await.expect_err("a drifting journal must not load");
    let message = format!("{err:#}");
    assert!(message.contains("MISMATCH Assets:Cathay:Savings TWD"), "{message}");
    assert!(message.contains("off by -1"), "{message}");

    let pool = db::init_db(&load_args.database_url).await?;
    let transactions: i64 =
        sqlx::query("SELECT COUNT(*) FROM transactions").fetch_one(&pool).await?.get(0);
    assert_eq!(transactions, 0, "a failed check must roll the whole load back");
    Ok(())
}

#[tokio::test]
async fn load_journal_requires_the_assertions_file() -> anyhow::Result<()> {
    let (_dir, freeze_args, load_args) = fixture();
    freeze::run(&freeze_args)?;
    std::fs::remove_file(journal::assertions_path(&freeze_args.journal))?;
    let err = load::run(&load_args).await.expect_err("an unchecked journal must not load");
    assert!(format!("{err:#}").contains("assertions.csv"), "{err:#}");

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
    assert!(frozen.to_string().contains("Assets:Empty-Wallet TWD = -600"), "{frozen}");
    assert!(!freeze_args.journal.exists(), "an untrusted journal must be removed");
    assert!(
        !journal::assertions_path(&freeze_args.journal).exists(),
        "its assertions must go with it"
    );
    Ok(())
}

/// A failed freeze overwrites nothing: the pair from the last good run is still
/// on disk, still verified, and no staged file is left behind.
#[test]
fn a_failed_freeze_leaves_the_last_verified_journal_in_place() -> anyhow::Result<()> {
    let (_dir, freeze_args, _load) = fixture();
    let assertions = journal::assertions_path(&freeze_args.journal);
    assert!(freeze::run(&freeze_args)?.ok());
    let good = std::fs::read_to_string(&freeze_args.journal)?;
    let good_assertions = std::fs::read_to_string(&assertions)?;

    // The same overdraw that fails the freeze in the test above.
    std::fs::write(
        freeze_args.build.ledger_dir.join("manual.csv"),
        "date,account,contra,amount,payee,narration\n2024-08-01,Assets:Empty-Wallet,Expenses:Food,\
         -600,overdraw,no funds\n",
    )?;
    assert!(!freeze::run(&freeze_args)?.ok(), "the overdraw should fail the freeze");

    assert_eq!(std::fs::read_to_string(&freeze_args.journal)?, good, "the good journal was lost");
    assert_eq!(std::fs::read_to_string(&assertions)?, good_assertions);
    let leftovers: Vec<_> = std::fs::read_dir(freeze_args.build.ledger_dir)?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|name| name.ends_with(".staged"))
        .collect();
    assert!(leftovers.is_empty(), "staged files left behind: {leftovers:?}");
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
[accounts]
"券商" = "Assets:Broker"
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
        mapping: root.join("mapping.toml"),
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

/// With the backfill, the opening it derives is what an opening for the bank
/// account must agree with, not the statement's own later figure.
#[test]
fn an_opening_row_is_checked_against_the_backfill_opening() -> anyhow::Result<()> {
    // 國泰 maps to the savings account itself, so an opening can name it.
    let mapping =
        MAPPING.replace(r#""國泰" = "Assets:Cathay""#, r#""國泰" = "Assets:Cathay:Savings""#);
    // Statement opens at 0 on 2024-03-01; backfill before it spends 200 on
    // 2022-02-01 and moves 3000 on 2022-05-01, so it derives an opening of 3200.
    for (amount, ok) in [("3200", true), ("0", false)] {
        let (_dir, args, _load) = fixture();
        std::fs::write(args.build.ledger_dir.join("mapping.toml"), &mapping)?;
        let records = format!(
            "{TRANSACTIONS}o:2,active,2022-01-01,,transfer,{amount},TWD,起鼓,國泰,{amount},TWD,,,,\
             ,,config,m,,added,,\n"
        );
        std::fs::write(args.build.ledger_dir.join("transactions.csv"), records)?;
        let result = freeze::run(&args);
        match ok {
            true => assert!(result?.ok(), "a correct opening was rejected"),
            false => {
                let err = result.expect_err("a contradicting opening must fail");
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

/// A journal leg as the freeze would write it, for hand-built journals.
fn leg(group: u64, account: &str, amount: Decimal) -> journal::Posting {
    journal::Posting {
        group,
        source: model::Source::Manual,
        date: NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(),
        payee: None,
        narration: String::new(),
        external_ref: None,
        account: account.into(),
        amount,
        currency: Currency::TWD,
        tags: None,
    }
}

/// What a hand-built journal adds up to. These tests exercise the loader's
/// tripwires rather than the invariant, but the gate still needs something to
/// check, and nothing outside the journal vouches for an invented one.
fn self_assertions(postings: &[journal::Posting]) -> Vec<BalanceAssertion> {
    let mut totals: std::collections::BTreeMap<(String, Currency), Decimal> = Default::default();
    for p in postings {
        *totals.entry((p.account.clone(), p.currency)).or_default() += p.amount;
    }
    totals
        .into_iter()
        .map(|((account, currency), closing)| BalanceAssertion {
            source: AssertionSource::Counted,
            account,
            currency,
            period_start: None,
            opening: None,
            period_end: NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(),
            closing,
        })
        .collect()
}

/// Writes `postings` as a journal and loads it into a fresh database.
async fn load_journal(postings: Vec<journal::Posting>) -> anyhow::Result<(TempDir, load::Report)> {
    let dir = TempDir::new()?;
    // The loader insists on a chart; these paths are not in it, so every label
    // falls back to the leaf.
    let mapping = dir.path().join("mapping.toml");
    std::fs::write(&mapping, "[display]\n")?;
    let args = load::Args {
        journal: dir.path().join("journal.csv"),
        database_url: format!("sqlite:{}", dir.path().join("ledger-app.db").display()),
        mapping,
    };
    let assertions = self_assertions(&postings);
    journal::write(&args.journal, &journal::Journal { postings })?;
    journal::write_assertions(&journal::assertions_path(&args.journal), &assertions)?;
    let report = load::run(&args).await?;
    Ok((dir, report))
}

/// The loader trusts the journal but not blindly: a group whose legs do not
/// sum to zero is a corrupt file, and loading it would break double entry.
#[tokio::test]
async fn the_journal_load_refuses_an_unbalanced_transaction() -> anyhow::Result<()> {
    let err =
        load_journal(vec![leg(0, "Assets:Cash", dec!(-120)), leg(0, "Expenses:Food", dec!(100))])
            .await
            .expect_err("an unbalanced group must not load");
    assert!(format!("{err:#}").contains("does not balance: -20 TWD"), "wrong error: {err:#}");
    Ok(())
}

/// Every account's type comes from its root; a path without a known root has
/// none.
#[tokio::test]
async fn the_journal_load_types_accounts_by_their_root() -> anyhow::Result<()> {
    let (dir, report) = load_journal(vec![
        leg(0, "Liabilities:Card", dec!(-120)),
        leg(0, "Expenses:Food", dec!(120)),
    ])
    .await?;
    // The two the journal names, plus the roots they hang from: every tree
    // level is an account, so the UI has a labelled row for it.
    assert_eq!(report.accounts, 4);
    let pool =
        db::init_db(&format!("sqlite:{}", dir.path().join("ledger-app.db").display())).await?;
    let kind: String = sqlx::query("SELECT type FROM accounts WHERE path = 'Liabilities:Card'")
        .fetch_one(&pool)
        .await?
        .get(0);
    assert_eq!(kind, "liability");

    let err =
        load_journal(vec![leg(0, "Spending:Food", dec!(120)), leg(0, "Assets:Cash", dec!(-120))])
            .await
            .expect_err("a rootless account must not load");
    // The root is what has no type, and the root is the account reported: the
    // loader reaches "Spending" before the leaf hanging off it.
    assert!(format!("{err:#}").contains("\"Spending\" is not a Beancount account"), "{err:#}");
    Ok(())
}

/// Runs `hledger check --strict` on a journal; hledger comes from `nix
/// develop`.
fn hledger_check(journal: &std::path::Path) -> std::process::Output {
    std::process::Command::new("hledger")
        .args(["check", "--strict", "-f"])
        .arg(journal)
        .output()
        .expect("hledger not on PATH; run the tests inside `nix develop`")
}

/// The export is an independent audit: hledger must parse it, find every
/// transaction balanced and every assertion true, and must catch a wrong one.
#[tokio::test]
async fn the_hledger_export_passes_hledger_check_and_catches_drift() -> anyhow::Result<()> {
    let (dir, freeze_args, load_args) = fixture();
    freeze::run(&freeze_args)?;
    load::run(&load_args).await?;
    let pool = db::init_db(&load_args.database_url).await?;
    let path = dir.path().join("ledger.journal");

    // A counted assertion in a currency no posting mentions must still declare
    // its commodity, or --strict rejects the export.
    assertions::insert(&mut *pool.acquire().await?, &BalanceAssertion {
        source: AssertionSource::Counted,
        account: "Assets:Cash".into(),
        currency: portfolio::currency::Currency::USD,
        period_start: None,
        opening: None,
        period_end: NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
        closing: dec!(0),
    })
    .await?;

    let exported = store::hledger::export(&mut *pool.acquire().await?).await?;
    assert!(exported.contains("=* 4900 TWD"), "statement closing not exported:\n{exported}");
    assert!(exported.contains("commodity USD"), "undeclared commodity:\n{exported}");
    std::fs::write(&path, &exported)?;
    let out = hledger_check(&path);
    assert!(
        out.status.success(),
        "hledger check failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    sqlx::query("UPDATE balance_assertion SET closing = '4901' WHERE source = 'statement'")
        .execute(&pool)
        .await?;
    std::fs::write(&path, store::hledger::export(&mut *pool.acquire().await?).await?)?;
    let out = hledger_check(&path);
    assert!(!out.status.success(), "hledger accepted a wrong balance assertion");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Balance assertion failed"));
    Ok(())
}

/// An empty assertions file must not wave the history through: the gate has
/// nothing to check, so the load fails and commits nothing.
#[tokio::test]
async fn load_journal_refuses_an_empty_assertions_file() -> anyhow::Result<()> {
    let (_dir, freeze_args, load_args) = fixture();
    freeze::run(&freeze_args)?;
    std::fs::write(journal::assertions_path(&freeze_args.journal), "")?;

    let err = load::run(&load_args).await.expect_err("nothing vouches for this journal");
    assert!(format!("{err:#}").contains("nothing vouches"), "{err:#}");
    let pool = db::init_db(&load_args.database_url).await?;
    let transactions: i64 =
        sqlx::query("SELECT COUNT(*) FROM transactions").fetch_one(&pool).await?.get(0);
    assert_eq!(transactions, 0, "unvouched history must not commit");
    Ok(())
}

/// If the journal cannot be moved into place, the assertions already moved must
/// not be left describing the previous journal.
#[test]
fn a_half_finished_move_leaves_no_mismatched_pair() -> anyhow::Result<()> {
    let (_dir, freeze_args, _load) = fixture();
    let assertions = journal::assertions_path(&freeze_args.journal);
    // A non-empty directory in the journal's place: the rename cannot succeed.
    std::fs::create_dir(&freeze_args.journal)?;
    std::fs::write(freeze_args.journal.join("occupied"), "")?;

    let err = freeze::run(&freeze_args).expect_err("the journal could not be moved into place");
    assert!(format!("{err:#}").contains("re-run freeze"), "{err:#}");
    assert!(!assertions.exists(), "assertions left beside a journal they do not describe");
    assert!(!staged(&freeze_args.journal).exists(), "the staged journal was left behind");
    Ok(())
}

/// The staged name freeze writes under, mirrored from the runner.
fn staged(journal: &std::path::Path) -> std::path::PathBuf {
    let mut name = journal.file_name().unwrap_or_default().to_os_string();
    name.push(".staged");
    journal.with_file_name(name)
}
