//! LINE Bank through freeze and import, on invented statements: T4d's two
//! acceptance checks, plus what the freeze makes of a transfer from the other
//! bank, a called-back transfer and an idle USD account.
//!
//! The statements are given as the text `pdftotext -raw` prints; a stand-in
//! `pdftotext` on PATH serves `<name>.txt` for `<name>.pdf`, so the real
//! subprocess path runs without a PDF in the repo.

use std::{collections::BTreeMap, path::PathBuf, sync::Once};

use anyhow::Result;
use db::load;
use import::bank::{import, Report};
use ledger::{accounts::Chart, args::Args as BuildArgs, freeze, journal, statements::bank::Bank};
use ledger_types::assertion::AssertionSource;
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
"444444444444" = "Assets:LineBank"

[fallback]
income  = "Income:Uncategorized"
expense = "Expenses:Uncategorized"

[expenses]
"飲食" = "Expenses:Food"

[accounts]
"國泰" = "Assets:Cathay"
"LINE" = "Assets:LineBank"
"現金" = "Assets:Cash"
"券商" = "Assets:Broker"
"起鼓" = "Equity:Opening-Balances"
"#;

const SAVINGS: &str = "\
111111111111 活存
幣別：TWD
交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註
2026/01/05,2026/01/05,電子轉出,500,,500,(824)0000444444444444,
2025/12/01,2025/12/01,存入,,1000,1000,,
";

/// A page's rows end on a line of control characters, as the PDF's
/// decorative layer renders; the slash-dated rows are that layer's samples.
const SEPARATOR: &str = "\u{1}\u{2} \u{3}\u{4}";

fn statement(period: &str, closing: &str, rows: &str) -> String {
    with_usd(period, closing, rows, "0.00")
}

fn with_usd(period: &str, closing: &str, rows: &str, usd: &str) -> String {
    format!(
        "LINE Bank 連線商業銀行 對帳單\n對帳單期間:{period}\n1 / \
         2台幣存款總餘額\n{closing}\n台幣存款交易明細\n主帳戶 *******44444 {closing}\n日期 \
         交易說明 交易金額 餘額 備註\n{rows}{SEPARATOR}\n2019/10/09 ATM -40,000 $199,960,000 \
         0130017T\n $837,992\n範例頁尾 02-0000-0000\n2 / \
         2外幣存款總餘額\n$0\n外幣存款交易明細\n美元 *******00099 {usd} \
         USD\n本月無交易紀錄\n簽帳金融卡交易明細\n本月無交易紀錄\n"
    )
}

/// January: the transfer from 國泰, a friend's transfer called back and sent
/// again, and a purchase. Opens on 100 the records carried in.
fn january() -> String {
    statement(
        "20260101-20260131",
        "$250",
        "2026.01.05 轉帳 $500 $600\n範例銀行 ***********\n11111\n2026.01.10 LINE好友轉帳 -$300 \
         $300 阿明\n2026.01.10 取消轉帳 $300 $600 取消.阿明\n2026.01.10 LINE好友轉帳 -$300 $300 \
         阿明\n2026.01.12 消費 -$50 $250 範例店\n",
    )
}

/// February: interest on a row that prints no balance.
fn february() -> String {
    statement("20260201-20260228", "$255", "2026.02.03 存款利息 $5 利息\n")
}

const RECORDS: &str = "\
id,status,date,posted_date,kind,amount,currency,account,counter_account,counter_amount,\
                       counter_currency,category,major_category,member,tags,note,source_party,\
                       source_file,source_id,origin,correction_note,updated_at
o:1,active,2025-11-30,,transfer,1000,TWD,起鼓,現金,1000,TWD,,,,,,config,m,,added,,
a:1,active,2025-12-01,,transfer,100,TWD,現金,LINE,100,TWD,,,,,,app,x,U1,raw,,
a:2,active,2025-12-01,,income,1000,TWD,國泰,,,,薪資,,,,,app,x,U2,raw,,
a:3,active,2026-01-05,,transfer,500,TWD,國泰,LINE,500,TWD,,,,,,app,x,U3,raw,,
a:4,active,2026-01-10,,transfer,300,TWD,LINE,現金,300,TWD,,,,,,app,x,U4,raw,,
a:5,active,2026-01-12,,expense,50,TWD,LINE,,,,飲食,,,,,app,x,U5,raw,,
a:6,active,2026-03-05,,expense,20,TWD,LINE,,,,飲食,,,,,app,x,U6,raw,,
";

static FAKE_PDFTOTEXT: Once = Once::new();

/// Puts a `pdftotext` first on PATH that prints the `.txt` beside the `.pdf`.
fn fake_pdftotext() {
    FAKE_PDFTOTEXT.call_once(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fake-pdftotext");
        std::fs::create_dir_all(&dir).expect("fake pdftotext dir");
        let script = dir.join("pdftotext");
        // Called as `pdftotext -raw -enc UTF-8 <file> -`.
        std::fs::write(&script, "#!/bin/sh\nexec cat \"${4%.pdf}.txt\"\n").expect("script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(dir).chain(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).expect("PATH"));
    });
}

struct Fixture {
    dir: TempDir,
    chart: Chart,
}

impl Fixture {
    fn new() -> Result<Self> {
        fake_pdftotext();
        let dir = TempDir::new()?;
        std::fs::write(dir.path().join("mapping.toml"), MAPPING)?;
        std::fs::create_dir(dir.path().join("活存-444444444444"))?;
        let chart = Chart::load(dir.path().join("mapping.toml"))?;
        Ok(Fixture { dir, chart })
    }

    fn write(&self, name: &str, contents: &str) -> Result<PathBuf> {
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    fn line_bank(&self) -> Result<Vec<PathBuf>> {
        self.write("活存-444444444444/2026-02.txt", &february())?;
        self.write("活存-444444444444/2026-01.txt", &january())?;
        let folder = self.dir.path().join("活存-444444444444");
        Ok(vec![folder.join("2026-02.pdf"), folder.join("2026-01.pdf")])
    }

    fn freeze_args(&self) -> Result<freeze::FreezeArgs> {
        self.freeze_with(vec![self.write("savings.csv", SAVINGS)?], RECORDS)
    }

    fn freeze_with(&self, cathay: Vec<PathBuf>, records: &str) -> Result<freeze::FreezeArgs> {
        Ok(freeze::FreezeArgs {
            journal: self.dir.path().join("journal.csv"),
            build: BuildArgs {
                cathay_statements: cathay,
                line_bank_statements: self.line_bank()?,
                daily_income_expense: None,
                daily_transfers: None,
                transactions: Some(self.write("transactions.csv", records)?),
                backfill: false,
                ledger_dir: self.dir.path().to_path_buf(),
            },
        })
    }

    async fn db(&self, name: &str) -> Result<SqlitePool> {
        db::init_db(&format!("sqlite:{}", self.dir.path().join(name).display())).await
    }

    async fn import(&self, pool: &SqlitePool, bank: Bank, paths: &[PathBuf]) -> Result<Report> {
        let mut tx = pool.begin().await?;
        let report = import(&mut tx, &self.chart, bank, &bank.load_merged(paths)?, &[]).await?;
        tx.commit().await?;
        Ok(report)
    }
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

#[test]
fn the_freeze_checks_every_month_against_its_statement() -> Result<()> {
    let f = Fixture::new()?;
    let args = f.freeze_args()?;
    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "{frozen}");

    let asserted = journal::read_assertions(&journal::assertions_path(&args.journal))?;
    let line: Vec<_> = asserted
        .iter()
        .filter(|a| a.account == "Assets:LineBank" && a.source == AssertionSource::Statement)
        .map(|a| (a.currency.to_string(), a.opening, a.period_end.to_string(), a.closing))
        .collect();
    assert_eq!(line, [
        ("TWD".into(), Some(dec!(100)), "2026-01-31".into(), dec!(250)),
        ("TWD".into(), Some(dec!(250)), "2026-02-28".into(), dec!(255)),
        ("USD".into(), Some(dec!(0)), "2026-01-31".into(), dec!(0)),
        ("USD".into(), Some(dec!(0)), "2026-02-28".into(), dec!(0)),
    ]);

    let written = journal::read(&args.journal)?;
    let on_line = |p: &&journal::Posting| p.account == "Assets:LineBank";
    // The records before the statement carry its opening; nothing is invented.
    assert!(written.postings.iter().filter(on_line).all(|p| p.currency.to_string() == "TWD"));
    let groups_touching = |account: &str| -> Vec<u64> {
        let mut g: Vec<u64> =
            written.postings.iter().filter(|p| p.account == account).map(|p| p.group).collect();
        g.dedup();
        g
    };
    let opening = groups_touching("Equity:Opening-Balances");
    assert!(written.postings.iter().filter(on_line).all(|p| !opening.contains(&p.group)));

    // Both halves of the cancelled attempt stay, booked together.
    let called_back: Vec<Decimal> = written
        .postings
        .iter()
        .filter(on_line)
        .filter(|p| p.payee.as_deref() == Some("取消轉帳"))
        .map(|p| p.amount)
        .collect();
    assert_eq!(called_back, [dec!(-300), dec!(300)]);

    // The transfer from 國泰 is one transaction from both statements; its app
    // record is spent on it, not left over as unmatched.
    let transfer: Vec<(String, Decimal)> = written
        .postings
        .iter()
        .filter(|p| p.date.to_string() == "2026-01-05")
        .map(|p| (p.account.clone(), p.amount))
        .collect();
    assert_eq!(transfer, [
        ("Assets:Cathay:Savings".to_string(), dec!(-500)),
        ("Assets:LineBank".to_string(), dec!(500)),
    ]);
    assert!(written.postings.iter().all(|p| p.payee.as_deref() != Some("未對應紀錄")));

    // Newer than the last statement: on the account, tagged unverified.
    let march: Vec<(String, Decimal, Option<String>)> = written
        .postings
        .iter()
        .filter(|p| p.date.to_string() == "2026-03-05")
        .map(|p| (p.account.clone(), p.amount, p.tags.clone()))
        .collect();
    let unverified = Some(journal::UNVERIFIED_TAG.to_string());
    assert_eq!(march, [
        ("Expenses:Food".to_string(), dec!(20), unverified.clone()),
        ("Assets:LineBank".to_string(), dec!(-20), unverified),
    ]);
    Ok(())
}

/// Acceptance (a), synthetic: a ledger loaded from the freeze already holds
/// every LINE Bank line and every Cathay one.
#[tokio::test]
async fn a_frozen_ledger_holds_every_line() -> Result<()> {
    let f = Fixture::new()?;
    let args = f.freeze_args()?;
    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "{frozen}");
    let url = format!("sqlite:{}", f.dir.path().join("frozen.db").display());
    load::run(&load::Args {
        journal: args.journal.clone(),
        database_url: url.clone(),
        mapping: f.dir.path().join("mapping.toml"),
    })
    .await?;
    let pool = db::init_db(&url).await?;
    let unreviewed: Vec<String> =
        sqlx::query_scalar("SELECT date FROM transactions WHERE reviewed = 0")
            .fetch_all(&pool)
            .await?;
    assert_eq!(unreviewed, ["2026-03-05"]);

    let report = f.import(&pool, Bank::LineBank, &args.build.line_bank_statements).await?;
    assert_eq!(report.inserted, 0, "{report}");
    assert!(report.check.ok(), "{report}");
    // The far half of the transfer from 國泰, and the call-back, are booked
    // with their partners.
    assert_eq!((report.counts.known, report.counts.covered), (4, 2), "{report}");

    let report = f.import(&pool, Bank::Cathay, &args.build.cathay_statements).await?;
    assert_eq!(report.inserted, 0, "{report}");
    assert!(report.check.ok(), "{report}");
    Ok(())
}

/// Acceptance (b), synthetic: into an empty ledger every statement closes, and
/// the idle USD account is asserted at zero with no postings.
#[tokio::test]
async fn an_empty_ledger_closes_on_every_statement() -> Result<()> {
    let f = Fixture::new()?;
    let pool = f.db("empty.db").await?;
    let report = f.import(&pool, Bank::LineBank, &f.line_bank()?).await?;
    assert!(report.check.ok(), "{report}");
    assert_eq!(report.counts.openings, 1);
    assert_eq!(report.check.figures(), 8, "{report}");

    let b = balances(&pool).await?;
    let bal = |account: &str, ccy: &str| {
        b.get(&(account.to_string(), ccy.to_string())).copied().unwrap_or_default()
    };
    assert_eq!(bal("Assets:LineBank", "TWD"), dec!(255));
    assert!(!b.contains_key(&("Assets:LineBank".to_string(), "USD".to_string())));
    // Named 國泰, whose statement is not in this import: in transit.
    assert_eq!(bal("Assets:Cathay:Clearing", "TWD"), dec!(-500));
    let opening: String = sqlx::query_scalar(
        "SELECT date FROM transactions WHERE external_ref = \
         'line-bank:opening:Assets:LineBank:TWD'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(opening, "2025-12-31");

    let again = f.import(&pool, Bank::LineBank, &f.line_bank()?).await?;
    assert_eq!(again.inserted, 0, "{again}");
    Ok(())
}

/// A statement that starts before the records do: its lines before them are
/// cut, and the month is checked from where the kept lines start.
#[test]
fn a_statement_older_than_the_records_is_checked_from_where_they_begin() -> Result<()> {
    let f = Fixture::new()?;
    let records: String = RECORDS
        .lines()
        .filter(|l| !l.starts_with("a:1,") && !l.starts_with("a:2,") && !l.starts_with("a:3,"))
        .map(|l| format!("{l}\n"))
        .collect();
    let args = f.freeze_with(Vec::new(), &records)?;
    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "{frozen}");
    let asserted = journal::read_assertions(&journal::assertions_path(&args.journal))?;
    let january = asserted
        .iter()
        .find(|a| a.account == "Assets:LineBank" && a.period_end.to_string() == "2026-01-31")
        .expect("January asserted");
    assert_eq!(
        (january.period_start.map(|d| d.to_string()), january.opening),
        (Some("2026-01-10".to_string()), Some(dec!(600)))
    );
    Ok(())
}

/// An idle account holding money opens on it, in freeze and in import, and
/// every month's check holds.
#[tokio::test]
async fn an_idle_account_with_a_balance_opens_on_it() -> Result<()> {
    let f = Fixture::new()?;
    f.write(
        "活存-444444444444/2026-01.txt",
        &with_usd("20260101-20260131", "$50", "2026.01.12 轉帳 $50 $50 範例店\n", "100.00"),
    )?;
    let paths = vec![f.dir.path().join("活存-444444444444/2026-01.pdf")];
    let pool = f.db("idle.db").await?;
    let report = f.import(&pool, Bank::LineBank, &paths).await?;
    assert!(report.check.ok(), "{report}");
    let b = balances(&pool).await?;
    assert_eq!(b.get(&("Assets:LineBank".to_string(), "USD".to_string())), Some(&dec!(100.00)));
    let again = f.import(&pool, Bank::LineBank, &paths).await?;
    assert_eq!(again.inserted, 0, "{again}");

    let args = freeze::FreezeArgs {
        journal: f.dir.path().join("journal.csv"),
        build: BuildArgs {
            cathay_statements: Vec::new(),
            line_bank_statements: paths,
            daily_income_expense: None,
            daily_transfers: None,
            transactions: Some(f.write(
                "transactions.csv",
                RECORDS.lines().take(2).collect::<Vec<_>>().join("\n").as_str(),
            )?),
            backfill: false,
            ledger_dir: f.dir.path().to_path_buf(),
        },
    };
    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "{frozen}");
    let usd: Decimal = journal::read(&args.journal)?
        .postings
        .iter()
        .filter(|p| p.account == "Assets:LineBank" && p.currency.to_string() == "USD")
        .map(|p| p.amount)
        .sum();
    assert_eq!(usd, dec!(100.00));
    Ok(())
}

/// The statement that arrives after an unverified record replaces it: the
/// line books with the record's category, on the bank's date, and neither
/// copy is left over. A re-import then holds the line by its ref.
#[tokio::test]
async fn the_next_statement_verifies_an_unverified_record() -> Result<()> {
    let f = Fixture::new()?;
    let args = f.freeze_args()?;
    let frozen = freeze::run(&args)?;
    assert!(frozen.ok(), "{frozen}");
    let url = format!("sqlite:{}", f.dir.path().join("verify.db").display());
    load::run(&load::Args {
        journal: args.journal.clone(),
        database_url: url.clone(),
        mapping: f.dir.path().join("mapping.toml"),
    })
    .await?;
    let pool = db::init_db(&url).await?;

    // The record says 03-05; the bank booked it 03-06. 03-10 has no record.
    f.write(
        "活存-444444444444/2026-03.txt",
        &statement(
            "20260301-20260331",
            "$250",
            "2026.03.06 消費 -$20 $235 範例店\n2026.03.10 轉帳 $15 $250 範例銀行\n",
        ),
    )?;
    let march = vec![f.dir.path().join("活存-444444444444/2026-03.pdf")];
    let report = f.import(&pool, Bank::LineBank, &march).await?;
    assert!(report.check.ok(), "{report}");
    assert_eq!((report.inserted, report.counts.replaced, report.counts.new), (2, 1, 1), "{report}");

    let b = balances(&pool).await?;
    let bal = |account: &str| {
        b.get(&(account.to_string(), "TWD".to_string())).copied().unwrap_or_default()
    };
    assert_eq!(bal("Assets:LineBank"), dec!(250));
    assert_eq!(bal("Expenses:Food"), dec!(70));
    let tagged: i64 =
        sqlx::query_scalar("SELECT count(*) FROM postings WHERE tags LIKE '%unverified%'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(tagged, 0);
    let dates: Vec<String> = sqlx::query_scalar(
        "SELECT t.date FROM transactions t JOIN postings p ON p.transaction_id = t.id
         JOIN accounts a ON a.id = p.account_id WHERE a.path = 'Expenses:Food' ORDER BY t.date",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(dates, ["2026-01-12", "2026-03-06"]);

    let again = f.import(&pool, Bank::LineBank, &march).await?;
    assert_eq!(again.inserted, 0, "{again}");
    Ok(())
}
