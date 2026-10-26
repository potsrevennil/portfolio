//! Each way a 天天記帳 or bank import stops, on invented records over two
//! pooled bank accounts the app knows only together as 國泰: refused and
//! rolled back, kept for the review queue, fixed, and imported again.

mod common;

use std::path::{Path, PathBuf};

use anyhow::Result;
use common::{
    cathay::{period, Line},
    tiantian::{memoed, EXPORT_HEADER, TRANSFER_HEADER},
};
use db::{
    review::{self, Edit, LegEdit, Manual, StatementAccounts},
    stops::{self, Stop},
};
use import::{
    bank,
    tiantian::{self, Files},
};
use ledger::{accounts::Chart, corrected, daily, statements::bank::Bank};
use sqlx::SqlitePool;
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
"333333333333" = "Assets:Cathay:Investment"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[expenses]
"飲食" = "Expenses:Food"

[accounts]
"國泰" = "Assets:Cathay"
"券商" = "Assets:Broker"
"現金" = "Assets:Cash"
"#;

const SAVINGS: &str = "111111111111";
const INVESTMENT: &str = "333333333333";

/// The first download of each account: one spend on January 2.
const OPENING: &[Line] = &[("2026/01/02", "消費", "100", "", "900", "", "")];

struct Fixture {
    dir: TempDir,
    chart: Chart,
    pool: SqlitePool,
}

impl Fixture {
    /// Each account opened by its first download, through January 10.
    async fn new() -> Result<Self> {
        let dir = TempDir::new()?;
        std::fs::write(dir.path().join("mapping.toml"), MAPPING)?;
        let chart = Chart::load(dir.path().join("mapping.toml"))?;
        std::fs::write(dir.path().join("frozen.csv"), "date,source_id\n")?;
        let url = format!("sqlite:{}", dir.path().join("ledger.db").display());
        let pool = db::init_db(&url).await?;
        let f = Fixture { dir, chart, pool };
        for account in [SAVINGS, INVESTMENT] {
            f.import_bank(&[(account, "2026/01/11", OPENING)]).await?;
        }
        Ok(f)
    }

    fn write(&self, name: &str, contents: &str) -> Result<PathBuf> {
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    async fn import_bank(&self, downloads: &[(&str, &str, &[Line<'_>])]) -> Result<bank::Report> {
        let mut paths = Vec::new();
        for (account, to, lines) in downloads {
            let name = format!("{account}-{}.csv", to.replace('/', ""));
            paths.push(self.write(&name, &period(account, Some(to), lines))?);
        }
        let merged = Bank::Cathay.load_merged(&paths)?;
        bank::import_all(&self.pool, &self.chart, Bank::Cathay, &merged, &[]).await
    }

    async fn import_tiantian(&self, records: &[String]) -> Result<tiantian::Report> {
        let transfers = self.write("轉帳.csv", TRANSFER_HEADER)?;
        self.import_tiantian_with(records, &transfers).await
    }

    /// Imports `records` beside the 轉帳 export at `transfers`.
    async fn import_tiantian_with(
        &self,
        records: &[String],
        transfers: &Path,
    ) -> Result<tiantian::Report> {
        let export: String =
            [EXPORT_HEADER].into_iter().chain(records.iter().map(String::as_str)).collect();
        let income_expense = self.write("收支.csv", &export)?;
        let entries = daily::load_entries(&income_expense, transfers)?;
        let frozen = corrected::frozen(self.dir.path().join("frozen.csv"))?;
        let files = Files { income_expense: &income_expense, transfers };
        tiantian::import_all(&self.pool, &self.chart, &entries, &frozen, files).await
    }

    async fn transactions(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM transactions").fetch_one(&self.pool).await?)
    }

    /// The one stop open, which must be the only one.
    async fn the_stop(&self) -> Result<Stop> {
        let mut open = stops::open(&self.pool).await?;
        assert_eq!(open.len(), 1, "{open:?}");
        Ok(open.remove(0))
    }

    /// The transaction a statement line on `account` dated `date` stands for.
    async fn line(&self, account: &str, date: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT t.id FROM transactions t JOIN postings p ON p.transaction_id = t.id
             JOIN accounts a ON a.id = p.account_id
             WHERE a.path = ? AND t.date = ? AND t.external_ref LIKE 'cathay-bank:%'",
        )
        .bind(account)
        .bind(date)
        .fetch_one(&self.pool)
        .await?)
    }
}

/// 活存 through January, with a spend on the 25th.
const SAVINGS_JANUARY: &[Line] = &[OPENING[0], ("2026/01/25", "消費", "50", "", "850", "", "")];
/// 投資 through January, with a spend on the 20th.
const INVESTMENT_JANUARY: &[Line] = &[OPENING[0], ("2026/01/20", "消費", "200", "", "700", "", "")];

/// The 國泰 pool books a spend on 活存, but the bank shows it on 投資: the
/// statements vouch for its day and no 活存 line is it. Nothing is written,
/// the stop names the 投資 line it may be, and once the export is fixed the
/// import goes through and the stop is gone.
#[tokio::test]
async fn a_record_no_line_explains_is_kept_until_the_export_is_fixed() -> Result<()> {
    let f = Fixture::new().await?;
    let january =
        [(SAVINGS, "2026/02/01", SAVINGS_JANUARY), (INVESTMENT, "2026/02/01", INVESTMENT_JANUARY)];
    f.import_bank(&january).await?;
    let line = f.line("Assets:Cathay:Investment", "2026-01-20").await?;
    let before = f.transactions().await?;

    f.import_tiantian(&[memoed("20260120", 200, "國泰", "U1", "午餐")])
        .await
        .expect_err("no 活存 line is U1");
    assert_eq!(f.transactions().await?, before);
    let Stop::RecordUnexplained { record, through, lines } = f.the_stop().await? else {
        panic!("not unexplained")
    };
    assert_eq!(
        (record.account.as_str(), through.to_string(), lines),
        ("Assets:Cathay:Savings", "2026-01-31".to_string(), vec![line])
    );

    f.import_tiantian(&[memoed("20260125", 50, "國泰", "U2", "午餐")]).await?;
    assert!(stops::open(&f.pool).await?.is_empty());
    Ok(())
}

/// A line another record of the same export pairs with is not named: the
/// record that pairs with it keeps it.
#[tokio::test]
async fn a_line_another_record_pairs_with_is_not_named() -> Result<()> {
    let f = Fixture::new().await?;
    let january =
        [(SAVINGS, "2026/02/01", SAVINGS_JANUARY), (INVESTMENT, "2026/02/01", INVESTMENT_JANUARY)];
    f.import_bank(&january).await?;
    // U2 is the 投資 line, booked through 券商; U1 on 活存 has no line.
    let settled = "20260120,國泰,200,TWD,券商,200,TWD,,,x,U2\n";
    let transfers = f.write("轉帳.csv", &[TRANSFER_HEADER, settled].concat())?;
    let records = [memoed("20260120", 200, "國泰", "U1", "午餐")];
    f.import_tiantian_with(&records, &transfers).await.expect_err("no 活存 line is U1");
    let Stop::RecordUnexplained { lines, .. } = f.the_stop().await? else {
        panic!("not unexplained")
    };
    assert!(lines.is_empty(), "U2 takes the 投資 line: {lines:?}");
    Ok(())
}

/// Two lines of the record's amount within the window: nothing chooses, and
/// the stop names both.
#[tokio::test]
async fn an_ambiguous_record_names_its_lines() -> Result<()> {
    let f = Fixture::new().await?;
    let savings: &[Line] = &[
        OPENING[0],
        ("2026/01/18", "消費", "200", "", "700", "", ""),
        ("2026/01/20", "消費", "200", "", "500", "", ""),
    ];
    f.import_bank(&[(SAVINGS, "2026/02/01", savings)]).await?;
    let lines = vec![
        f.line("Assets:Cathay:Savings", "2026-01-18").await?,
        f.line("Assets:Cathay:Savings", "2026-01-20").await?,
    ];
    f.import_tiantian(&[memoed("20260120", 200, "國泰", "U1", "午餐")])
        .await
        .expect_err("two lines of 200");
    assert!(matches!(f.the_stop().await?, Stop::RecordAmbiguous { lines: l, .. } if l == lines));
    Ok(())
}

/// Recorded before the statements reached it, the spend waits on 活存; the
/// 活存 statement passes it by and only 投資's line fits it. The import moves
/// it to 投資, where that line verifies it.
#[tokio::test]
async fn an_unverified_record_on_the_wrong_sibling_moves_to_it() -> Result<()> {
    let f = Fixture::new().await?;
    f.import_tiantian(&[memoed("20260120", 200, "國泰", "U1", "午餐")]).await?;
    let before = f.transactions().await?;

    let january =
        [(SAVINGS, "2026/02/01", SAVINGS_JANUARY), (INVESTMENT, "2026/02/01", INVESTMENT_JANUARY)];
    let report = f.import_bank(&january).await?;
    assert_eq!((report.counts.moved, report.counts.verified), (1, 1), "{report}");
    // The 活存 spend is the only line added: the 投資 one is U1.
    assert_eq!(f.transactions().await?, before + 1);
    let on: String = sqlx::query_scalar(
        "SELECT a.path FROM postings p JOIN accounts a ON a.id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE t.external_ref = 'tiantian:U1' AND a.path LIKE 'Assets:%'",
    )
    .fetch_one(&f.pool)
    .await?;
    assert_eq!(on, "Assets:Cathay:Investment");
    Ok(())
}

/// A record whose one line of its amount a person already categorised: the
/// import ties it to that line, which keeps their category.
#[tokio::test]
async fn a_record_pairs_with_the_one_line_already_categorised() -> Result<()> {
    let f = Fixture::new().await?;
    let savings: &[Line] = &[OPENING[0], ("2026/01/20", "消費", "200", "", "700", "", "")];
    f.import_bank(&[(SAVINGS, "2026/02/01", savings)]).await?;
    let line = f.line("Assets:Cathay:Savings", "2026-01-20").await?;
    // The category a person gives it is one the ledger already has.
    let lunch = "20260105,飲食,食食,80,TWD,自己,現金,,,支,x,U0\n".to_string();
    f.import_tiantian(std::slice::from_ref(&lunch)).await?;
    let legs: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT p.id, a.path, p.amount FROM postings p JOIN accounts a ON a.id = p.account_id
         WHERE p.transaction_id = ?",
    )
    .bind(line)
    .fetch_all(&f.pool)
    .await?;
    let legs = legs
        .into_iter()
        .map(|(id, path, amount)| LegEdit {
            posting_id: Some(id),
            account: path.replace("Expenses:Uncategorized", "Expenses:Food"),
            amount: amount.parse().unwrap(),
            currency: "TWD".parse().unwrap(),
        })
        .collect();
    let edit = Edit { narration: None, legs, confirm: true };
    review::edit(&f.pool, &StatementAccounts::from(&f.chart), line, &edit).await?;

    let report = f.import_tiantian(&[lunch, memoed("20260120", 200, "國泰", "U1", "午餐")]).await?;
    assert_eq!(report.counts.paired, 1, "{report}");
    let refs: Vec<String> =
        sqlx::query_scalar("SELECT external_ref FROM transaction_refs WHERE transaction_id = ?")
            .bind(line)
            .fetch_all(&f.pool)
            .await?;
    assert_eq!(refs, ["tiantian:U1"]);
    let category: (String, String) = sqlx::query_as(
        "SELECT a.path, p.origin FROM postings p JOIN accounts a ON a.id = p.account_id
         WHERE p.transaction_id = ? AND a.path LIKE 'Expenses:%'",
    )
    .bind(line)
    .fetch_one(&f.pool)
    .await?;
    assert_eq!(category, ("Expenses:Food".to_string(), "manual".to_string()));
    Ok(())
}

/// A transfer entered by hand that 活存 books just before midnight and 投資
/// just after: it stays one transaction, on the day both statements agree
/// with, and the import goes through.
#[tokio::test]
async fn a_transfer_two_banks_book_a_day_apart_stays_one_transaction() -> Result<()> {
    let f = Fixture::new().await?;
    let manual = Manual {
        date: "2026-01-20".parse()?,
        amount: "300".parse()?,
        currency: "TWD".parse()?,
        account: "Assets:Cathay:Savings".into(),
        category: None,
        counter: Some("Assets:Cathay:Investment".into()),
        note: Some("轉帳".into()),
    };
    let transfer = review::enter(&f.pool, &StatementAccounts::from(&f.chart), &manual).await?;

    let savings: &[Line] = &[OPENING[0], ("2026/01/20", "轉出", "300", "", "600", "", "")];
    let investment: &[Line] = &[OPENING[0], ("2026/01/21", "轉入", "", "300", "1200", "", "")];
    let report = f
        .import_bank(&[(SAVINGS, "2026/02/01", savings), (INVESTMENT, "2026/02/01", investment)])
        .await?;
    assert_eq!(report.counts.verified, 2, "{report}");
    let date: String = sqlx::query_scalar("SELECT date FROM transactions WHERE id = ?")
        .bind(transfer)
        .fetch_one(&f.pool)
        .await?;
    assert_eq!(date, "2026-01-20");
    Ok(())
}
