//! 天天記帳 exports after the freeze, on invented records: overlap with the
//! frozen history and with an earlier import, a record on the bank account
//! either waiting for its line or relabelling the line already imported, a
//! transfer between two statement accounts, one the statements contradict,
//! and counted balances.

use std::path::PathBuf;

use anyhow::Result;
use db::{check, load};
use import::{
    bank,
    counted::record,
    tiantian::{import, Files, Report},
};
use ledger::{
    accounts::Chart, args::Args as BuildArgs, corrected, daily, freeze, statements::bank::Bank,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
"222222222222" = "Assets:Cathay:FX"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[expenses]
"飲食" = "Expenses:Food"

[income]
"薪資" = "Income:Salary"

[accounts]
"國泰" = "Assets:Cathay"
"現金"     = "Assets:Cash:TWD"
"外幣帳戶" = "Assets:Cathay:FX"
"券商" = "Assets:Broker"
"起鼓" = "Equity:Opening-Balances"

[counted]
roots = ["Assets:Cash"]
"#;

/// The frozen history: it ends on 2026-01-06.
const RECORDS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                       counter_currency,category,major_category,member,tags,note,source_party,\
                       source_file,source_id,origin,correction_note,updated_at
o:1,active,2025-12-31,,transfer,500,TWD,起鼓,現金,500,TWD,,,,,,config,m,,added,,
a:1,active,2026-01-02,,income,1000,TWD,國泰,,,,薪資,,,,,app,x,U1,raw,,
a:2,active,2026-01-05,,expense,100,TWD,國泰,,,,飲食,,,,,app,x,U2,raw,,
a:3,active,2026-01-06,,expense,50,TWD,現金,,,,飲食,,,,,app,x,U3,raw,,
";

const EXPORT_HEADER: &str =
    "日期,類別,大類別,金額,幣別,成員,帳戶,標籤,備註,收支區分,上次更新,UUID\n";
const TRANSFER_HEADER: &str =
    "日期,從帳戶,轉出金額,幣別,到帳戶,轉入金額,幣別,標籤,備註,上次更新,UUID\n";

fn flow(date: &str, amount: u32, account: &str, id: &str) -> String {
    memoed(date, amount, account, id, "")
}

fn memoed(date: &str, amount: u32, account: &str, id: &str, memo: &str) -> String {
    format!("{date},飲食,食食,{amount},TWD,自己,{account},,{memo},支,{date},{id}\n")
}

/// A later export: the frozen records, one entered late (U4), and three new:
/// cash (U5), and two on the bank account, a purchase and a withdrawal.
fn export() -> (String, String) {
    let income_expense = [
        EXPORT_HEADER.to_string(),
        "20260102,薪資,收入,1000,TWD,自己,國泰,,,收,x,U1\n".to_string(),
        flow("20260105", 100, "國泰", "U2"),
        flow("20260106", 50, "現金", "U3"),
        flow("20260105", 30, "現金", "U4"),
        flow("20260108", 20, "現金", "U5"),
        memoed("20260120", 200, "國泰", "U6", "咖啡"),
    ]
    .concat();
    let transfers = format!(
        "{TRANSFER_HEADER}20260121,國泰,300,TWD,現金,300,TWD,,領現,x,U7\n20260122,國泰,320,TWD,\
         外幣帳戶,10,USD,,,x,U8\n"
    );
    (income_expense, transfers)
}

/// A line as (date, 說明, 提出, 存入, 餘額, 備註), oldest first.
type Line<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str, &'a str);

const THROUGH_JANUARY_10: &[Line] =
    &[("2026/01/02", "存入", "", "1000", "1000", ""), ("2026/01/05", "消費", "100", "", "900", "")];
const THROUGH_JANUARY: &[Line] = &[
    ("2026/01/02", "存入", "", "1000", "1000", ""),
    ("2026/01/05", "消費", "100", "", "900", ""),
    ("2026/01/20", "消費", "200", "", "700", ""),
    ("2026/01/21", "自行提款", "300", "", "400", "無卡提款"),
    ("2026/01/22", "網銀外存", "320", "", "80", "222222222222"),
];

/// The 外幣 account's own statement, holding the far half of that transfer.
const FX_JANUARY: &str = "\
\"222222222222 活存外幣\"
\"幣別：USD\"
\"
\"
\"交易日期\",\"帳務日期\",\"提出\",\"存入\",\"餘額\",\"成交匯率\",\"交易資訊\"
\"2026/01/22\",\"2026/01/22\",\"−\",\"USD 10.00\",\"USD 10.00\",\"32\",\"台幣存 111111111111TWD\"
";

/// A dated download, newest line first; it vouches through the day before
/// `to`.
fn statement(to: &str, lines: &[Line]) -> String {
    let columns = ["交易日期", "帳務日期", "說明", "提出", "存入", "餘額", "交易資訊", "備註"]
        .map(|c| format!("\"{c}\""))
        .join(",");
    let mut out = format!(
        "\u{feff}\"111111111111 活存\"\r\n\"筆數\",\"(自 2025/12/01 至 \
         {to})\"\r\n\"幣別：TWD\"\r\n\"\r\n\"\r\n{columns}\r\n"
    );
    for (date, desc, out_, in_, bal, memo) in lines.iter().rev() {
        out.push_str(&format!(
            "\"{date}\",\"{date}\",\"{desc}\",\"{out_}\",\"{in_}\",\"{bal}\",\"\",\"{memo}\"\r\n"
        ));
    }
    out
}

struct Fixture {
    dir: TempDir,
    chart: Chart,
    pool: SqlitePool,
}

impl Fixture {
    /// A ledger loaded from a freeze of `RECORDS` and the statement through
    /// January 10.
    async fn frozen() -> Result<Self> {
        let dir = TempDir::new()?;
        std::fs::write(dir.path().join("mapping.toml"), MAPPING)?;
        let chart = Chart::load(dir.path().join("mapping.toml"))?;
        let write = |name: &str, contents: &str| -> Result<PathBuf> {
            let path = dir.path().join(name);
            std::fs::write(&path, contents)?;
            Ok(path)
        };
        let args = freeze::FreezeArgs {
            journal: dir.path().join("journal.csv"),
            build: BuildArgs {
                cathay_statements: vec![write(
                    "2026-01-11.csv",
                    &statement("2026/01/11", THROUGH_JANUARY_10),
                )?],
                line_bank_statements: Vec::new(),
                daily_income_expense: None,
                daily_transfers: None,
                transactions: Some(write("transactions.csv", RECORDS)?),
                backfill: false,
                ledger_dir: dir.path().to_path_buf(),
            },
        };
        let frozen = freeze::run(&args)?;
        assert!(frozen.ok(), "{frozen}");
        let url = format!("sqlite:{}", dir.path().join("ledger.db").display());
        load::run(&load::Args {
            journal: args.journal,
            database_url: url.clone(),
            mapping: dir.path().join("mapping.toml"),
        })
        .await?;
        let (income_expense, transfers) = export();
        write("收支.csv", &income_expense)?;
        write("轉帳.csv", &transfers)?;
        write("2026-02-01.csv", &statement("2026/02/01", THROUGH_JANUARY))?;
        write("2026-02-01-USD.csv", FX_JANUARY)?;
        let pool = db::init_db(&url).await?;
        Ok(Fixture { dir, chart, pool })
    }

    fn path(&self, name: &str) -> PathBuf { self.dir.path().join(name) }

    async fn import_tiantian(&self) -> Result<Report> {
        let entries = daily::load_entries(self.path("收支.csv"), self.path("轉帳.csv"))?;
        let frozen = corrected::frozen(self.path("transactions.csv"))?;
        let (ie, xf) = (self.path("收支.csv"), self.path("轉帳.csv"));
        let files = Files { income_expense: &ie, transfers: &xf };
        let mut tx = self.pool.begin().await?;
        let report = import(&mut tx, &self.chart, &entries, &frozen, files).await?;
        tx.commit().await?;
        Ok(report)
    }

    async fn import_january(&self) -> Result<bank::Report> {
        let merged = Bank::Cathay
            .load_merged(&[self.path("2026-02-01.csv"), self.path("2026-02-01-USD.csv")])?;
        let mut tx = self.pool.begin().await?;
        let report = bank::import(&mut tx, &self.chart, Bank::Cathay, &merged, &[]).await?;
        tx.commit().await?;
        Ok(report)
    }

    async fn transactions(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM transactions").fetch_one(&self.pool).await?)
    }

    /// The legs booked on `date`, by account, openings aside.
    async fn legs_on(&self, date: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "SELECT a.path, p.amount FROM postings p JOIN accounts a ON a.id = p.account_id
             JOIN transactions t ON t.id = p.transaction_id
             WHERE t.date = ? AND coalesce(t.payee, '') <> 'Opening balance' ORDER BY a.path",
        )
        .bind(date)
        .fetch_all(&self.pool)
        .await?)
    }

    /// What the transaction booked on `date` narrates.
    async fn narration(&self, date: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT narration FROM transactions WHERE date = ? AND coalesce(payee, '') <> \
             'Opening balance'",
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await?)
    }

    async fn cash(&self) -> Result<Decimal> {
        let amounts: Vec<String> = sqlx::query_scalar(
            "SELECT p.amount FROM postings p JOIN accounts a ON a.id = p.account_id
             WHERE a.path = 'Assets:Cash:TWD'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(amounts.iter().map(|a| a.parse::<Decimal>().unwrap()).sum())
    }
}

/// Only what the frozen history lacks goes in, and only once: a second import
/// of an overlapping export inserts nothing.
#[tokio::test]
async fn an_overlapping_export_inserts_only_the_new_records_once() -> Result<()> {
    let f = Fixture::frozen().await?;
    let before = f.transactions().await?;

    let report = f.import_tiantian().await?;
    assert_eq!(report.counts.frozen, 3, "{report}");
    // The late record is named, so the user knows what to add to corrected/.
    assert_eq!(report.late, [("U4".to_string(), "2026-01-05".parse()?)], "{report}");
    assert_eq!(report.counts.left_to_statements, 1, "{report}");
    assert_eq!((report.counts.standalone, report.counts.unverified), (1, 2), "{report}");
    assert!(report.check.ok(), "{report}");
    assert_eq!(f.transactions().await?, before + 3);
    assert_eq!(
        report.span.map(|(a, b)| (a.to_string(), b.to_string())),
        Some(("2026-01-08".into(), "2026-01-21".into()))
    );
    let unreviewed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM transactions WHERE reviewed = 0 AND external_ref LIKE 'tiantian:%'",
    )
    .fetch_one(&f.pool)
    .await?;
    assert_eq!(unreviewed, 3);

    let again = f.import_tiantian().await?;
    assert_eq!(again.counts.known, 3, "{again}");
    assert_eq!(again.counts.standalone + again.counts.unverified + again.counts.paired, 0);
    assert_eq!(f.transactions().await?, before + 3);
    assert_eq!(f.cash().await?, dec!(730));
    Ok(())
}

/// Recorded before its bank line: the record waits on the account, and the
/// statement then verifies it in place instead of adding the line.
#[tokio::test]
async fn a_bank_record_waits_for_its_line() -> Result<()> {
    let f = Fixture::frozen().await?;
    f.import_tiantian().await?;
    let before = f.transactions().await?;

    // The two records waiting on 國泰 are verified; what the import inserts is
    // the 外幣 account's opening and the conversion no record claims.
    let bank = f.import_january().await?;
    assert_eq!((bank.inserted, bank.counts.verified), (2, 2), "{bank}");
    assert!(bank.check.ok(), "{bank}");
    assert_eq!(f.transactions().await?, before + 2);
    assert_eq!(f.legs_on("2026-01-20").await?, [
        ("Assets:Cathay:Savings".to_string(), "-200".to_string()),
        ("Expenses:Food".to_string(), "200".to_string()),
    ]);

    let again = f.import_tiantian().await?;
    assert_eq!(again.counts.known, 3, "{again}");
    assert_eq!(again.counts.left_to_statements, 1, "{again}");
    assert_eq!(f.transactions().await?, before + 2);
    Ok(())
}

/// Recorded after its bank line was imported: the record relabels the line's
/// fallback leg, and adds nothing to the bank account.
#[tokio::test]
async fn a_bank_record_relabels_the_line_already_imported() -> Result<()> {
    let f = Fixture::frozen().await?;
    let bank = f.import_january().await?;
    assert_eq!(bank.counts.uncategorised, 2, "{bank}");
    let before = f.transactions().await?;
    // The transfer between the two statement accounts is theirs: both halves
    // are booked once, from the statements.
    assert_eq!(f.legs_on("2026-01-22").await?, [
        ("Assets:Cathay:FX".to_string(), "10.00".to_string()),
        ("Assets:Cathay:Savings".to_string(), "-320".to_string()),
        ("Equity:Conversions".to_string(), "320".to_string()),
        ("Equity:Conversions".to_string(), "-10.00".to_string()),
    ]);

    let report = f.import_tiantian().await?;
    assert_eq!((report.counts.paired, report.counts.standalone), (2, 1), "{report}");
    assert_eq!(report.counts.left_to_statements, 1, "{report}");
    assert!(report.check.ok(), "{report}");
    assert_eq!(f.transactions().await?, before + 1);
    assert_eq!(f.legs_on("2026-01-20").await?, [
        ("Assets:Cathay:Savings".to_string(), "-200".to_string()),
        ("Expenses:Food".to_string(), "200".to_string()),
    ]);
    // The review queue says the category came from the matched record.
    let origin: Option<String> = sqlx::query_scalar(
        "SELECT p.origin FROM postings p JOIN accounts a ON a.id = p.account_id JOIN transactions \
         t ON t.id = p.transaction_id WHERE t.date = '2026-01-20' AND a.path = 'Expenses:Food'",
    )
    .fetch_one(&f.pool)
    .await?;
    assert_eq!(origin.as_deref(), Some("tiantian"));
    assert_eq!(f.legs_on("2026-01-21").await?, [
        ("Assets:Cash:TWD".to_string(), "300".to_string()),
        ("Assets:Cathay:Savings".to_string(), "-300".to_string()),
    ]);
    // Nothing of the cross-account transfer was booked by the record.
    assert_eq!(f.legs_on("2026-01-22").await?.len(), 4);
    // Each record's own 備註 reaches the line it relabelled, beside what the
    // bank called it.
    assert_eq!(f.narration("2026-01-20").await?.as_deref(), Some("咖啡"), "{report}");
    assert_eq!(f.narration("2026-01-21").await?.as_deref(), Some("無卡提款 · 領現"), "{report}");

    let again = f.import_tiantian().await?;
    assert_eq!(again.counts.known, 3, "{again}");
    assert_eq!(f.transactions().await?, before + 1);
    assert_eq!(f.cash().await?, dec!(730));
    // Nothing was said twice by the second run.
    assert_eq!(f.narration("2026-01-20").await?.as_deref(), Some("咖啡"), "{again}");
    assert_eq!(f.narration("2026-01-21").await?.as_deref(), Some("無卡提款 · 領現"), "{again}");
    Ok(())
}

/// A bank record on a day the statements cover, with no line for it, would
/// double-count or invent money: the import stops and writes nothing.
#[tokio::test]
async fn a_bank_record_the_statement_contradicts_stops_the_import() -> Result<()> {
    let f = Fixture::frozen().await?;
    f.import_january().await?;
    let (mut income_expense, transfers) = export();
    income_expense.push_str(&flow("20260112", 999, "國泰", "U8"));
    std::fs::write(f.path("收支.csv"), income_expense)?;
    std::fs::write(f.path("轉帳.csv"), transfers)?;
    let before = f.transactions().await?;

    let err = f.import_tiantian().await.expect_err("no line explains U8");
    assert!(format!("{err:#}").contains("U8"), "{err:#}");
    assert_eq!(f.transactions().await?, before);
    Ok(())
}

/// Until the first count, cash is listed as never counted but passes; a
/// count the postings disagree with is refused, one they agree with holds.
#[tokio::test]
async fn a_count_is_held_to_the_postings() -> Result<()> {
    let f = Fixture::frozen().await?;
    let mut conn = f.pool.acquire().await?;
    let report = check::with_counts(&mut conn, &f.chart).await?;
    assert!(report.ok(), "{report}");
    assert_eq!(report.uncounted, [("Assets:Cash:TWD".to_string(), "TWD".parse()?)]);
    drop(conn);

    let day = "2026-01-06".parse()?;
    let mut tx = f.pool.begin().await?;
    let wrong = record(&mut tx, &f.chart, "Assets:Cash:TWD", day, dec!(460), None).await;
    let err = wrong.expect_err("the ledger holds 450");
    assert!(format!("{err:#}").contains("MISMATCH"), "{err:#}");
    tx.rollback().await?;
    let counted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM balance_assertion WHERE source = 'counted'")
            .fetch_one(&f.pool)
            .await?;
    assert_eq!(counted, 0);

    let mut tx = f.pool.begin().await?;
    let right = record(&mut tx, &f.chart, "Assets:Cash:TWD", day, dec!(450), None).await?;
    tx.commit().await?;
    assert!(right.check.uncounted.is_empty(), "{right}");

    // Imports are held to it from now on.
    let imported = f.import_tiantian().await?;
    assert!(imported.check.ok(), "{imported}");
    assert!(imported.check.uncounted.is_empty(), "{imported}");

    let mut tx = f.pool.begin().await?;
    let bank = record(&mut tx, &f.chart, "Assets:Cathay:Savings", day, dec!(900), None).await;
    assert!(bank.is_err(), "a bank account is checked by its statements");
    Ok(())
}
