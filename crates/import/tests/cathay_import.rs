//! Cathay bank import into SQLite, on invented statements: the synthetic
//! versions of T4a's two acceptance checks, plus the dedup-key fragility.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use db::load;
use import::{
    bank::{import, Report},
    plan::Candidate,
};
use ledger::{accounts::Chart, args::Args as BuildArgs, freeze, statements::bank::Bank};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::{Row, SqlitePool};
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
"222222222222" = "Assets:Cathay:FX"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[fallback.descriptions]
"存款息" = "Income:Interest"

[expenses]
"飲食" = "Expenses:Food"

[accounts]
"國泰"     = "Assets:Cathay"
"外幣帳戶" = "Assets:Cathay:FX"
"現金"     = "Assets:Cash"
"券商"     = "Assets:Broker"
"起鼓"     = "Equity:Opening-Balances"
"#;

/// A line as (date, 說明, 提出, 存入, 餘額, 交易資訊, 備註), oldest first.
type Line<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str, &'a str, &'a str);

/// A TWD export as the bank writes it: BOM, CRLF, the lone-quote spacer
/// record, newest line first.
fn twd(account: &str, lines: &[Line]) -> String { period(account, None, lines) }

/// The same, with the `(自 … 至 …)` header the bank puts on a dated download.
fn period(account: &str, period_end: Option<&str>, lines: &[Line]) -> String {
    let header = period_end
        .map(|end| format!("\"筆數\",\"(自 2020/01/01 至 {end})\"\r\n"))
        .unwrap_or_default();
    // The lone-quote spacer record, then the column header.
    let columns = ["交易日期", "帳務日期", "說明", "提出", "存入", "餘額", "交易資訊", "備註"]
        .map(|c| format!("\"{c}\""))
        .join(",");
    let mut out =
        format!("\u{feff}\"{account} 活存\"\r\n{header}\"幣別：TWD\"\r\n\"\r\n\"\r\n{columns}\r\n");
    for (date, desc, out_, in_, bal, info, memo) in lines.iter().rev() {
        out.push_str(&format!(
            "\"{date}\",\"{date}\",\"{desc}\",\"{out_}\",\"{in_}\",\"{bal}\",\"{info}\",\"{memo}\"\
             \r\n"
        ));
    }
    out
}

const SAVINGS_2022: &[Line] = &[("2022/12/01", "存入", "", "1000", "1000", "", "")];
const SAVINGS_2023: &[Line] = &[
    ("2023/02/01", "消費", "100", "", "900", "", ""),
    ("2023/02/03", "轉帳", "300", "", "600", "(013)0000333333333333", ""),
    // Out, back, out: the first and third share date, amount and balance.
    ("2023/02/05", "網銀轉帳", "500", "", "100", "(822)0000000000000001", ""),
    ("2023/02/05", "網銀轉帳", "", "500", "600", "(822)0000000000000001", ""),
    ("2023/02/05", "自行提款", "500", "", "100", "", ""),
    ("2023/02/07", "電子轉出", "50", "", "50", "(807)0000000000000002", ""),
    ("2023/02/07", "錯誤更正", "-50", "", "100", "(807)0000000000000002", ""),
    ("2023/02/10", "網銀外存", "64", "", "36", "", "222222222222"),
];
const SAVINGS_2024: &[Line] = &[("2024/03/01", "存款息", "", "1", "37", "", "")];
const INVESTMENT_2023: &[Line] =
    &[("2023/02/03", "轉入", "", "300", "300", "(013)0000111***111111", "")];

const FX_2023_USD: &str = "\
\"222222222222 活存外幣\"
\"幣別：USD\"
\"
\"
\"交易日期\",\"帳務日期\",\"提出\",\"存入\",\"餘額\",\"成交匯率\",\"交易資訊\"
\"2023/02/10\",\"2023/02/10\",\"−\",\"USD 2.00\",\"USD 2.00\",\"32\",\"台幣存 111111111111TWD\"
";

const RECORDS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                       counter_currency,category,major_category,member,tags,note,source_party,\
                       source_file,source_id,origin,correction_note,updated_at
o:1,active,2022-12-31,,transfer,100,TWD,起鼓,現金,100,TWD,,,,,,config,m,,added,,
a:1,active,2023-02-01,,expense,100,TWD,國泰,,,,飲食,,,,午餐,app,x,U1,raw,,
a:2,active,2023-02-10,,transfer,64,TWD,國泰,外幣帳戶,2,USD,,,,,換匯,app,x,U2,raw,,
";

struct Fixture {
    dir: TempDir,
    chart: Chart,
}

impl Fixture {
    fn new() -> Result<Self> {
        let dir = TempDir::new()?;
        std::fs::write(dir.path().join("mapping.toml"), MAPPING)?;
        let chart = Chart::load(dir.path().join("mapping.toml"))?;
        Ok(Fixture { dir, chart })
    }

    fn write(&self, name: &str, contents: &str) -> Result<PathBuf> {
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    /// Every download, one file per account per year.
    fn statements(&self) -> Result<Vec<PathBuf>> {
        Ok(vec![
            self.write("savings-2022.csv", &twd("111111111111", SAVINGS_2022))?,
            self.write("savings-2023.csv", &twd("111111111111", SAVINGS_2023))?,
            self.write("savings-2024.csv", &twd("111111111111", SAVINGS_2024))?,
            self.write("investment-2023.csv", &twd("333333333333", INVESTMENT_2023))?,
            self.write("fx-2023-USD.csv", FX_2023_USD)?,
        ])
    }

    async fn db(&self, name: &str) -> Result<SqlitePool> {
        let url = format!("sqlite:{}", self.dir.path().join(name).display());
        db::init_db(&url).await
    }
}

async fn import_files(
    pool: &SqlitePool,
    chart: &Chart,
    paths: &[PathBuf],
    candidates: &[Candidate],
) -> Result<Report> {
    let mut tx = pool.begin().await?;
    let merged = Bank::Cathay.load_merged(paths)?;
    let report = import(&mut tx, chart, Bank::Cathay, &merged, candidates).await?;
    tx.commit().await?;
    Ok(report)
}

async fn count(pool: &SqlitePool, sql: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(sql).fetch_one(pool).await?)
}

async fn balances(pool: &SqlitePool) -> Result<BTreeMap<(String, String), Decimal>> {
    let rows = sqlx::query(
        "SELECT a.path, p.currency, p.amount FROM postings p JOIN accounts a ON a.id = \
         p.account_id",
    )
    .fetch_all(pool)
    .await?;
    let mut out: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    for r in rows {
        let amount: String = r.get("amount");
        *out.entry((r.get("path"), r.get("currency"))).or_default() += amount.parse::<Decimal>()?;
    }
    Ok(out)
}

fn bal(b: &BTreeMap<(String, String), Decimal>, account: &str, ccy: &str) -> Decimal {
    b.get(&(account.to_string(), ccy.to_string())).copied().unwrap_or_default()
}

/// Acceptance (b), synthetic: into an empty ledger every account and currency
/// closes on its statement, with one opening each however many yearly files.
#[tokio::test]
async fn an_empty_ledger_closes_on_every_statement() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("empty.db").await?;
    let report = import_files(&pool, &f.chart, &f.statements()?, &[]).await?;
    assert!(report.check.ok(), "{report}");
    assert_eq!(report.counts.openings, 3);

    let b = balances(&pool).await?;
    assert_eq!(bal(&b, "Assets:Cathay:Savings", "TWD"), dec!(37));
    assert_eq!(bal(&b, "Assets:Cathay:Investment", "TWD"), dec!(300));
    assert_eq!(bal(&b, "Assets:Cathay:FX", "USD"), dec!(2.00));
    // Both halves of each transfer were paired, so nothing is in transit.
    assert_eq!(bal(&b, "Assets:Cathay:Clearing", "TWD"), dec!(0));
    // The reversal and its debit are one transaction netting to nothing.
    assert_eq!(bal(&b, "Expenses:Uncategorized", "TWD"), dec!(1100));
    assert_eq!(bal(&b, "Income:Interest", "TWD"), dec!(-1));

    // 12 lines, less the far halves of two transfers and one reversal, plus
    // three openings.
    assert_eq!(count(&pool, "SELECT count(*) FROM transactions").await?, 12);
    assert_eq!(count(&pool, "SELECT count(*) FROM transactions WHERE reviewed = 1").await?, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM transactions WHERE import_batch_id IS NULL").await?,
        0
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM import_batch").await?, 5);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM transactions WHERE external_ref NOT LIKE 'cathay-bank:%'"
        )
        .await?,
        0
    );

    // Importing the same downloads again writes nothing.
    let again = import_files(&pool, &f.chart, &f.statements()?, &[]).await?;
    assert_eq!(again.inserted, 0, "{again}");
    assert_eq!(count(&pool, "SELECT count(*) FROM import_batch").await?, 5);
    Ok(())
}

/// Acceptance (a), synthetic: a ledger loaded from the freeze journal already
/// holds every line, so re-importing all downloads inserts nothing.
#[tokio::test]
async fn a_frozen_ledger_holds_every_line() -> Result<()> {
    let f = Fixture::new()?;
    let statements = f.statements()?;
    let frozen = freeze::run(&freeze::FreezeArgs {
        journal: f.dir.path().join("journal.csv"),
        build: BuildArgs {
            cathay_statements: statements.clone(),
            line_bank_statements: Vec::new(),
            daily_income_expense: None,
            daily_transfers: None,
            transactions: Some(f.write("transactions.csv", RECORDS)?),
            backfill: false,
            ledger_dir: f.dir.path().to_path_buf(),
        },
    })?;
    assert!(frozen.ok(), "{frozen}");
    let url = format!("sqlite:{}", f.dir.path().join("frozen.db").display());
    load::run(&load::Args {
        journal: f.dir.path().join("journal.csv"),
        database_url: url.clone(),
        mapping: f.dir.path().join("mapping.toml"),
    })
    .await?;
    let pool = db::init_db(&url).await?;
    let before = count(&pool, "SELECT count(*) FROM transactions").await?;

    let report = import_files(&pool, &f.chart, &statements, &[]).await?;
    assert_eq!(report.inserted, 0, "{report}");
    assert_eq!(count(&pool, "SELECT count(*) FROM transactions").await?, before);
    // The 2022 line is inside the freeze's opening; the far halves are booked
    // with their partners.
    assert_eq!(report.counts.predate_opening, 1);
    assert_eq!(report.counts.covered, 3);
    assert!(report.check.ok(), "{report}");
    Ok(())
}

/// Files imported one at a time land exactly as all at once; a transfer whose
/// receiving side arrives first goes through the clearing account.
#[tokio::test]
async fn importing_file_by_file_matches_importing_together() -> Result<()> {
    let f = Fixture::new()?;
    let paths = f.statements()?;
    let pool = f.db("stepwise.db").await?;
    for i in [3, 0, 1, 2, 4] {
        import_files(&pool, &f.chart, &paths[i..=i], &[]).await?;
    }
    let b = balances(&pool).await?;
    assert_eq!(bal(&b, "Assets:Cathay:Savings", "TWD"), dec!(37));
    assert_eq!(bal(&b, "Assets:Cathay:Investment", "TWD"), dec!(300));
    assert_eq!(bal(&b, "Assets:Cathay:FX", "USD"), dec!(2.00));
    // Investment's receipt came before savings' send, so both halves passed
    // through clearing and cancel there.
    assert_eq!(bal(&b, "Assets:Cathay:Clearing", "TWD"), dec!(0));

    let again = import_files(&pool, &f.chart, &paths, &[]).await?;
    assert_eq!(again.inserted, 0, "{again}");
    Ok(())
}

/// A download that starts mid-day numbers its repeats from the wrong line;
/// the balance chain refuses it and nothing is written.
#[tokio::test]
async fn a_download_starting_mid_day_fails_loudly() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("midday.db").await?;
    // Held so far: the day up to the money coming back.
    let head = f.write("head.csv", &twd("111111111111", &SAVINGS_2023[..4]))?;
    import_files(&pool, &f.chart, &[head], &[]).await?;
    let before = count(&pool, "SELECT count(*) FROM transactions").await?;

    // The next download starts at the second withdrawal: its key has no `:2`,
    // so it looks like the first one, already held.
    let tail = f.write("tail.csv", &twd("111111111111", &SAVINGS_2023[4..]))?;
    let err = import_files(&pool, &f.chart, &[tail], &[]).await.expect_err("must not chain");
    assert!(format!("{err:#}").contains("starts mid-day"), "{err:#}");
    assert_eq!(count(&pool, "SELECT count(*) FROM transactions").await?, before);
    Ok(())
}

/// A download made during 2/05, which stops on a balance the full day
/// doesn't end on. Its half day must not be asserted as the day's close, or
/// the download that completes the day is refused.
fn partial(f: &Fixture) -> Result<PathBuf> {
    f.write("partial.csv", &period("111111111111", Some("2023/02/05"), &SAVINGS_2023[..4]))
}

/// The download that re-covers the partial one's days in full.
fn full(f: &Fixture) -> Result<PathBuf> { f.write("full.csv", &twd("111111111111", SAVINGS_2023)) }

#[tokio::test]
async fn a_download_ending_mid_day_is_completed_by_the_next() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("partial.db").await?;
    import_files(&pool, &f.chart, &[partial(&f)?], &[]).await?;
    let report = import_files(&pool, &f.chart, &[full(&f)?], &[]).await?;
    assert_eq!(report.counts.known, 4);
    assert!(report.check.ok(), "{report}");
    let b = balances(&pool).await?;
    assert_eq!(bal(&b, "Assets:Cathay:Savings", "TWD"), dec!(36));

    // The stale partial download, imported again, is a no-op.
    let again = import_files(&pool, &f.chart, &[partial(&f)?], &[]).await?;
    assert_eq!(again.inserted, 0, "{again}");
    Ok(())
}

/// `raw/` keeps both downloads, so importing it whole gives them together.
#[tokio::test]
async fn a_partial_download_and_its_replacement_import_together() -> Result<()> {
    let f = Fixture::new()?;
    let together = f.db("together.db").await?;
    import_files(&together, &f.chart, &[partial(&f)?, full(&f)?], &[]).await?;
    let alone = f.db("alone.db").await?;
    import_files(&alone, &f.chart, &[full(&f)?], &[]).await?;

    assert_eq!(balances(&together).await?, balances(&alone).await?);
    let n = "SELECT count(*) FROM transactions";
    assert_eq!(count(&together, n).await?, count(&alone, n).await?);
    Ok(())
}

/// Downloaded at noon on 2/06 with nothing posted yet that day, then again
/// that evening after something did. The noon download must not have closed
/// 2/06.
#[tokio::test]
async fn a_download_made_before_anything_posted_that_day_is_completed_later() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("noon.db").await?;
    let noon =
        f.write("noon.csv", &period("111111111111", Some("2023/02/06"), &SAVINGS_2023[..5]))?;
    import_files(&pool, &f.chart, &[noon], &[]).await?;

    let afternoon = ("2023/02/06", "消費", "20", "", "80", "", "");
    let evening = [&SAVINGS_2023[..5], &[afternoon][..]].concat();
    let evening = f.write("evening.csv", &period("111111111111", Some("2023/02/06"), &evening))?;
    let report = import_files(&pool, &f.chart, &[evening], &[]).await?;
    assert_eq!(report.inserted, 1, "{report}");
    assert!(report.check.ok(), "{report}");
    Ok(())
}

/// A 外幣 export states no range, and its last balance is taken as current.
/// Downloaded between a morning conversion and an afternoon one, it recorded
/// a half day: the later download contradicts it, and the import stops rather
/// than record a second closing for that day.
#[tokio::test]
async fn a_second_foreign_download_the_same_day_is_refused_loudly() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("fx.db").await?;
    import_files(&pool, &f.chart, &[f.write("morning.csv", FX_2023_USD)?], &[]).await?;
    let before = count(&pool, "SELECT count(*) FROM transactions").await?;

    let afternoon = FX_2023_USD.replace(
        "\"交易資訊\"\n",
        "\"交易資訊\"\n\"2023/02/10\",\"2023/02/10\",\"USD 0.50\",\"−\",\"USD \
         1.50\",\"−\",\"網銀轉\"\n",
    );
    let err = import_files(&pool, &f.chart, &[f.write("later.csv", &afternoon)?], &[])
        .await
        .expect_err("contradicts the recorded closing");
    assert!(format!("{err:#}").contains("stale"), "{err:#}");
    assert_eq!(count(&pool, "SELECT count(*) FROM transactions").await?, before);
    Ok(())
}

#[tokio::test]
async fn a_missing_download_fails_loudly() -> Result<()> {
    let f = Fixture::new()?;
    let paths = f.statements()?;
    let pool = f.db("gap.db").await?;
    import_files(&pool, &f.chart, &paths[..1], &[]).await?;
    let err = import_files(&pool, &f.chart, &paths[2..3], &[]).await.expect_err("2023 is missing");
    assert!(format!("{err:#}").contains("one is missing"), "{err:#}");
    Ok(())
}

/// A matched 天天記帳 record names the line's other side; the mapping's
/// description rules come next, then 未分類.
#[tokio::test]
async fn labels_come_from_the_matched_record_then_the_rules() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("labels.db").await?;
    let lunch = Candidate {
        date: "2023-02-01".parse()?,
        amount: dec!(-100),
        currency: ledger_types::currency::Currency::TWD,
        account: "Expenses:Food".into(),
    };
    let paths = f.statements()?;
    let report = import_files(&pool, &f.chart, &paths, &[lunch]).await?;
    assert_eq!(report.counts.matched, 1);
    let b = balances(&pool).await?;
    assert_eq!(bal(&b, "Expenses:Food", "TWD"), dec!(100));
    assert_eq!(bal(&b, "Income:Interest", "TWD"), dec!(-1));
    assert_eq!(bal(&b, "Expenses:Uncategorized", "TWD"), dec!(1000));
    Ok(())
}

/// Two downloads whose header period ends on the same day but which disagree
/// about the closing balance: one of them is stale, and the import says so
/// rather than recording a second figure for that day.
#[tokio::test]
async fn a_closing_figure_that_contradicts_a_recorded_one_is_refused() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("stale.db").await?;
    let short =
        f.write("short.csv", &period("111111111111", Some("2023/12/31"), &SAVINGS_2023[..3]))?;
    import_files(&pool, &f.chart, &[short], &[]).await?;

    let full = f.write("full.csv", &period("111111111111", Some("2023/12/31"), SAVINGS_2023))?;
    let err = import_files(&pool, &f.chart, &[full], &[]).await.expect_err("stale assertion");
    assert!(format!("{err:#}").contains("is stale"), "{err:#}");
    Ok(())
}
