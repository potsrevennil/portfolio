//! Broker statements into the broker sub-ledger, on invented statements:
//! overlapping and repeated downloads, openings, and the holding gate.

use std::{collections::HashMap, path::PathBuf};

use anyhow::Result;
use import::broker::{import, read, Report, Source};
use ledger::accounts::Chart;
use sqlx::SqlitePool;
use tempfile::TempDir;

const MAPPING: &str = r#"
[institution]
app_account            = "銀行"
primary                = "Assets:Bank:Savings"
settlement             = "Assets:Bank:Savings"
settlement_app_account = "證券"
clearing               = "Assets:Bank:Clearing"

[institution.accounts]
"U0000000" = "Assets:Broker:IB"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"
"#;

const IB: &str = "Assets:Broker:IB";

/// An invented IB statement: `body` rows, the ending cash and the positions
/// (symbol, quantity) held at the end.
fn ib(
    period: &str,
    generated: &str,
    body: &str,
    ending: &str,
    positions: &[(&str, &str)],
) -> String {
    let positions: String = positions
        .iter()
        .map(|(s, q)| format!("Open Positions,Data,Summary,Stocks,USD,{s},-,{q},1,1,1,1,1,0,\n"))
        .collect();
    format!(
        "Statement,Header,Field Name,Field Value
Statement,Data,Period,\"{period}\"
Statement,Data,WhenGenerated,\"{generated}\"
Account Information,Header,Field Name,Field Value
Account Information,Data,Account,U0000000
Account Information,Data,Base Currency,USD
Cash Report,Header,Currency Summary,Currency,Total,Securities,Futures,
Cash Report,Data,Starting Cash,Base Currency Summary,0,0,0,
Cash Report,Data,Ending Cash,Base Currency Summary,{ending},{ending},0,
Open Positions,Header,DataDiscriminator,Asset Category,Currency,Symbol,Open,Quantity,Mult,Cost \
         Price,Cost Basis,Close Price,Value,Unrealized P/L,Code
{positions}Mark-to-Market Performance Summary,Header,Asset Category,Symbol,Prior Quantity,Current \
         Quantity
Deposits & Withdrawals,Header,Currency,Account,Settle Date,Description,Amount
Trades,Header,DataDiscriminator,Asset Category,Currency,Account,Symbol,Date/Time,Quantity,T. \
         Price,C. Price,Proceeds,Comm/Fee,Basis,Realized P/L,MTM P/L,Code
Withholding Tax,Header,Currency,Account,Date,Description,Amount,Code
{body}"
    )
}

const DEPOSIT: &str =
    "Deposits & Withdrawals,Data,USD,U0000000,2025-01-10,Electronic Fund Transfer,100\n";
const BUY: &str =
    "Trades,Data,Order,Stocks,USD,U0000000,ZZA,\"2025-03-03, 10:00:00\",2,40,40,-80,-1,81,0,0,O\n";
const CHARGE: &str =
    "Withholding Tax,Data,USD,U0000000,2025-08-01,ZZA(US0000000001) Cash Dividend - US Tax,-0.5,\n";
const REFUND: &str =
    "Withholding Tax,Data,USD,U0000000,2025-08-01,ZZA(US0000000001) Cash Dividend - US Tax,0.5,\n";

fn half_year(generated: &str) -> String {
    ib("January 1, 2025 - June 30, 2025", generated, &format!("{DEPOSIT}{BUY}"), "19", &[(
        "ZZA", "2",
    )])
}

fn full_year() -> String {
    let body = format!("{DEPOSIT}{BUY}{CHARGE}{REFUND}{CHARGE}");
    ib("January 1, 2025 - December 31, 2025", "2026-01-08, 22:56:25 EST", &body, "18.5", &[(
        "ZZA", "2",
    )])
}

struct Fixture {
    dir: TempDir,
    chart: Chart,
}

impl Fixture {
    fn new() -> Result<Self> {
        let dir = TempDir::new()?;
        let mapping = dir.path().join("mapping.toml");
        std::fs::write(&mapping, MAPPING)?;
        Ok(Fixture { chart: Chart::load(&mapping)?, dir })
    }

    fn file(&self, name: &str, text: &str) -> Result<PathBuf> {
        let path = self.dir.path().join(name);
        std::fs::write(&path, text)?;
        Ok(path)
    }

    async fn db(&self) -> Result<SqlitePool> {
        db::init_db(&format!("sqlite:{}", self.dir.path().join("ledger.db").display())).await
    }

    /// Reads and imports `files` in one transaction, committed only on success.
    async fn import(&self, pool: &SqlitePool, files: &[PathBuf]) -> Result<(Report, Vec<PathBuf>)> {
        let (statements, superseded) = read(Source::Ib, files, &HashMap::new())?;
        let mut tx = pool.begin().await?;
        let report = import(&mut tx, &self.chart, Source::Ib, &statements, None).await?;
        tx.commit().await?;
        Ok((report, superseded))
    }
}

async fn count(pool: &SqlitePool, table: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!("SELECT count(*) FROM {table}")).fetch_one(pool).await?)
}

#[tokio::test]
async fn overlapping_downloads_store_each_line_once() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db().await?;
    let half = f.file("half.csv", &half_year("2025-07-01, 08:00:00 EDT"))?;
    let full = f.file("full.csv", &full_year())?;

    let (first, _) = f.import(&pool, std::slice::from_ref(&half)).await?;
    assert_eq!(first.inserted, 2);
    let (second, _) = f.import(&pool, &[half, full.clone()]).await?;
    assert_eq!((second.inserted, second.known), (3, 4), "only the second half-year is new");
    // A charge, its refund and the charge again: three lines, the repeat numbered.
    assert_eq!(count(&pool, "broker_record").await?, 5);
    assert!(second.check.ok(), "{}", second.check);

    let batches = count(&pool, "import_batch").await?;
    let (again, _) = f.import(&pool, &[full]).await?;
    assert_eq!(again.inserted, 0);
    assert_eq!(count(&pool, "import_batch").await?, batches, "no empty batch");
    Ok(())
}

#[tokio::test]
async fn of_two_downloads_of_one_period_the_newer_is_read() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db().await?;
    let older = f.file("older.csv", &half_year("2025-07-01, 08:00:00 EDT"))?;
    let newer = f.file("newer.csv", &half_year("2025-07-02, 08:00:00 EDT"))?;
    let (report, superseded) = f.import(&pool, &[newer.clone(), older.clone()]).await?;
    assert_eq!(superseded, [older]);
    assert_eq!(report.statements, 1);
    assert_eq!(report.inserted, 2);
    Ok(())
}

#[tokio::test]
async fn a_statement_the_records_dont_reach_fails_the_gate() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db().await?;
    // States three shares; the one trade bought two.
    let wrong = ib(
        "January 1, 2025 - June 30, 2025",
        "2025-07-01, 08:00:00 EDT",
        &format!("{DEPOSIT}{BUY}"),
        "19",
        &[("ZZA", "3")],
    );
    let err = f.import(&pool, &[f.file("wrong.csv", &wrong)?]).await.unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains(&format!("MISMATCH {IB} ZZA (statement as of 2025-06-30)")),
        "{message}"
    );
    assert!(message.contains("broker records sum to 2"), "{message}");
    assert_eq!(count(&pool, "broker_record").await?, 0, "nothing committed");
    Ok(())
}

#[tokio::test]
async fn a_position_sold_off_is_asserted_at_zero() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db().await?;
    f.import(&pool, &[f.file("half.csv", &half_year("2025-07-01, 08:00:00 EDT"))?]).await?;
    // Lists no positions, though ZZA was bought and never sold.
    let empty = ib("July 1, 2025 - December 31, 2025", "2026-01-08, 22:56:25 EST", "", "19", &[])
        .replace(
            "Starting Cash,Base Currency Summary,0,0",
            "Starting Cash,Base Currency Summary,19,19",
        )
        .replace(
            "Current Quantity\n",
            "Current Quantity\nMark-to-Market Performance Summary,Data,Stocks,ZZA,2,0\n",
        );
    let err = f.import(&pool, &[f.file("empty.csv", &empty)?]).await.unwrap_err();
    assert!(format!("{err:#}").contains("expected 0, broker records sum to 2"), "{err:#}");

    // An opening that contradicts the recorded closing is refused outright.
    let contradicting = empty.replace("Stocks,ZZA,2,0", "Stocks,ZZA,3,0");
    let err = f.import(&pool, &[f.file("contradicting.csv", &contradicting)?]).await.unwrap_err();
    assert!(format!("{err:#}").contains("states 3, but 2 was recorded"), "{err:#}");
    Ok(())
}

#[tokio::test]
async fn coverage_reads_both_assertion_tables() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db().await?;
    f.import(&pool, &[f.file("full.csv", &full_year())?]).await?;
    let coverage = db::holdings::coverage(&mut *pool.acquire().await?).await?;
    let rows: Vec<(&str, &str, &str, &str)> = coverage
        .iter()
        .map(|c| (c.account.as_str(), c.kind.as_str(), c.commodity.as_str(), c.as_of.as_str()))
        .collect();
    assert_eq!(rows, [(IB, "cash", "USD", "2025-12-31"), (IB, "position", "ZZA", "2025-12-31")]);
    Ok(())
}

const CATHAY_HEADER: &str = "根據您篩選的結果，總計有2筆資料
股名,日期,成交股數,淨收付金額,買賣別,成交價,成本,手續費,交易稅,融資金額/券擔保品,資自備款/券保證金,\
                             利息,稅款,券手續費/標借費,委託書號
";
const CATHAY_A: &str = "範例一,2022/12/26,10,\"-1,001\",現買,100,\"1,000\",1,0,0,0,0,0,0,A0001\n";
const CATHAY_B: &str = "範例一,2023/01/05,4,398,現賣,100,400,1,1,0,0,0,0,0,A0001\n";

#[tokio::test]
async fn overlapping_cathay_exports_store_each_trade_once() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db().await?;
    // Something must vouch for the ledger before the gate passes an import
    // that states no balances of its own.
    f.import(&pool, &[f.file("full.csv", &full_year())?]).await?;

    let wide = f.file("2022-2023.csv", &format!("\u{feff}{CATHAY_HEADER}{CATHAY_B}{CATHAY_A}"))?;
    let narrow = f.file("2022.csv", &format!("\u{feff}{CATHAY_HEADER}{CATHAY_A}"))?;
    let names = HashMap::from([("範例一".to_string(), "ZZ01.TW".to_string())]);
    let (statements, _) = read(Source::CathaySecurities, &[narrow, wide], &names)?;
    let mut tx = pool.begin().await?;
    // The same order number on two days is two trades.
    let report = import(
        &mut tx,
        &f.chart,
        Source::CathaySecurities,
        &statements,
        Some("Assets:Broker:Cathay"),
    )
    .await?;
    tx.commit().await?;
    assert_eq!((report.inserted, report.known, report.assertions), (2, 1, 0));
    Ok(())
}
