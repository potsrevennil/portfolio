//! The `freeze` and `load-journal` commands, run as the real binary: argument
//! parsing, dispatch and the exit status. All data invented.

use std::process::{Command, Output};

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

/// A savings statement whose last running balance is `closing`; 4900 is what
/// its lines add up to.
fn statement(closing: u32) -> String {
    format!(
        "111111111111 \
         活存\n幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n2024/06/10,2024/06/\
         10,午餐,100,,{closing},,\n2024/06/01,2024/06/01,存入,,5000,5000,,\n"
    )
}

fn portfolio(dir: &TempDir, args: &[&str]) -> anyhow::Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_portfolio")).current_dir(dir.path()).args(args).output()?)
}

fn freeze(dir: &TempDir, closing: u32) -> anyhow::Result<Output> {
    std::fs::write(dir.path().join("mapping.toml"), MAPPING)?;
    std::fs::write(dir.path().join("savings.csv"), statement(closing))?;
    portfolio(dir, &[
        "freeze",
        "--journal",
        "journal.csv",
        "--ledger-dir",
        ".",
        "--cathay-statements",
        "savings.csv",
    ])
}

#[test]
fn freeze_then_load_journal_from_the_command_line() -> anyhow::Result<()> {
    let dir = TempDir::new()?;

    let frozen = freeze(&dir, 4900)?;
    let stdout = String::from_utf8_lossy(&frozen.stdout);
    assert!(
        frozen.status.success(),
        "freeze failed:\n{stdout}{}",
        String::from_utf8_lossy(&frozen.stderr)
    );
    assert!(stdout.contains("froze reconciled history to journal.csv"), "{stdout}");

    let loaded = portfolio(&dir, &[
        "load-journal",
        "--journal",
        "journal.csv",
        "--database-url",
        "sqlite:app.db",
        "--mapping",
        "mapping.toml",
    ])?;
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    assert!(
        loaded.status.success(),
        "load failed:\n{stdout}{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(stdout.contains("loaded journal into SQLite"), "{stdout}");
    assert!(dir.path().join("app.db").exists());
    Ok(())
}

/// A failed reconciliation must fail the process, not just print a report, so
/// a script running the freeze stops there.
#[test]
fn a_failed_freeze_exits_non_zero() -> anyhow::Result<()> {
    let dir = TempDir::new()?;

    let frozen = freeze(&dir, 4800)?;
    assert!(!frozen.status.success(), "a mismatch must fail the command");
    assert!(String::from_utf8_lossy(&frozen.stdout).contains("MISMATCH"));
    assert!(String::from_utf8_lossy(&frozen.stderr).contains("reconciliation failed"));
    assert!(!dir.path().join("journal.csv").exists());
    Ok(())
}
