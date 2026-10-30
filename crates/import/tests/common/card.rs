//! A ledger frozen from invented 天天記帳 records, for a statement that
//! vouches for them to be imported over. Statements are written by each test;
//! this holds what they share.

use std::path::PathBuf;

use anyhow::Result;
use db::{check, import_batch, load};
use import::bank::{import, Report};
use ledger::{accounts::Chart, args::Args as BuildArgs, freeze, statements::bank::Bank};
use ledger_types::{assertion::BalanceAssertion, currency::Currency};
use sqlx::SqlitePool;
use tempfile::TempDir;

pub const CARD: &str = "Liabilities:Card:Test";
pub const BANK: &str = "Assets:Bank:Second";
/// The folder its statements are in.
pub const BANK_FOLDER: &str = "活存-222222222222";

const MAPPING: &str = r#"
[institution]
app_account            = "銀行"
primary                = "Assets:Bank:Main"
settlement             = "Assets:Bank:Main"
settlement_app_account = "券商"
clearing               = "Assets:Bank:Clearing"

[institution.accounts]
"111111111111" = "Assets:Bank:Main"
"testcard"     = "Liabilities:Card:Test"
"222222222222" = "Assets:Bank:Second"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[expenses]
"飲食" = "Expenses:Food"
"其他" = "Expenses:Other"

[income]
"回饋" = "Income:Rebates"

[accounts]
"銀行" = "Assets:Bank:Main"
"測試卡" = "Liabilities:Card:Test"
"二銀" = "Assets:Bank:Second"
"券商" = "Assets:Broker"
"起鼓" = "Equity:Opening-Balances"
"#;

/// The freeze needs a bank statement to reconcile.
const SAVINGS: &str = "\
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2025/12/01,2025/12/01,存入,,1000,1000,,
";

const HEADER: &str = "id,status,date,posted_date,kind,amount,currency,account,counter_account,\
                      counter_amount,counter_currency,category,major_category,member,tags,note,\
                      source_party,source_file,source_id,origin,correction_note,updated_at\n";

/// `date, kind, amount, category`, one record each on the card.
pub fn records(rows: &[(&str, &str, u32, &str)]) -> String { records_on("測試卡", rows) }

/// One record each on the app account `account`.
pub fn records_on(account: &str, rows: &[(&str, &str, u32, &str)]) -> String {
    let mut csv = HEADER.to_string();
    csv.push_str("s:1,active,2025-12-01,,income,1000,TWD,銀行,,,,回饋,,,,,app,x,S1,raw,,\n");
    for (n, (date, kind, amount, category)) in rows.iter().enumerate() {
        csv.push_str(&format!(
            "r:{n},active,{date},,{kind},{amount},TWD,{account},,,,{category},,,,,app,x,U{n},raw,,\
             \n"
        ));
    }
    csv
}

pub struct Fixture {
    pub dir: TempDir,
    pub chart: Chart,
    pub pool: SqlitePool,
}

impl Fixture {
    /// A ledger frozen and loaded from `records`.
    pub async fn frozen(records: &str) -> Result<Self> {
        let dir = TempDir::new()?;
        std::fs::write(dir.path().join("mapping.toml"), MAPPING)?;
        std::fs::write(dir.path().join("transactions.csv"), records)?;
        std::fs::write(dir.path().join("savings.csv"), SAVINGS)?;
        std::fs::create_dir(dir.path().join("testcard"))?;
        std::fs::create_dir(dir.path().join(BANK_FOLDER))?;
        let journal = dir.path().join("journal.csv");
        let frozen = freeze::run(&freeze::FreezeArgs {
            journal: journal.clone(),
            build: BuildArgs {
                cathay_statements: vec![dir.path().join("savings.csv")],
                line_bank_statements: vec![],
                daily_income_expense: None,
                daily_transfers: None,
                transactions: Some(dir.path().join("transactions.csv")),
                backfill: false,
                ledger_dir: dir.path().to_path_buf(),
            },
        })?;
        anyhow::ensure!(frozen.ok(), "freeze failed:\n{frozen}");
        let url = format!("sqlite:{}", dir.path().join("frozen.db").display());
        load::run(&load::Args {
            journal,
            database_url: url.clone(),
            market_url: format!("{url}.market"),
            mapping: dir.path().join("mapping.toml"),
        })
        .await?;
        let pool = db::init_db(&url).await?;
        let chart = Chart::load(dir.path().join("mapping.toml"))?;
        Ok(Fixture { dir, chart, pool })
    }

    /// Writes a statement into the card's folder.
    pub fn write(&self, name: &str, contents: &str) -> Result<PathBuf> {
        let path = self.dir.path().join("testcard").join(name);
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    pub async fn import(&self, bank: Bank, paths: &[PathBuf]) -> Result<Report> {
        let merged = bank.load_merged(paths)?;
        let mut tx = self.pool.begin().await?;
        let report = import(&mut tx, &self.chart, bank, &merged, &[]).await?;
        tx.commit().await?;
        Ok(report)
    }

    /// `(transaction date, posted date, amount)` of the card's legs.
    pub async fn legs(&self) -> Result<Vec<(String, Option<String>, String)>> {
        Ok(sqlx::query_as(
            "SELECT t.date, p.posted_date, p.amount FROM postings p
             JOIN transactions t ON t.id = p.transaction_id
             JOIN accounts a ON a.id = p.account_id
             WHERE a.path = ? ORDER BY coalesce(p.posted_date, t.date), t.date, p.id",
        )
        .bind(CARD)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn figures(&self) -> Result<Vec<BalanceAssertion>> {
        let mut conn = self.pool.acquire().await?;
        let mut figures = db::assertions::load(&mut conn).await?;
        figures.retain(|a| a.account == CARD);
        Ok(figures)
    }

    pub async fn unverified(&self) -> Result<usize> {
        let mut conn = self.pool.acquire().await?;
        let legs = import_batch::postings(&mut conn, CARD, Currency::TWD).await?;
        Ok(legs.iter().filter(|p| p.unverified).count())
    }

    pub async fn gate_passes(&self) -> Result<bool> {
        Ok(check::check(&mut *self.pool.acquire().await?).await?.ok())
    }
}
