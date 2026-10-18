//! Each way an import stops, on invented records over three pooled bank
//! accounts the app knows only together as 國泰: refused and rolled back, kept
//! for the review queue, settled there, and imported again.

use std::path::PathBuf;

use anyhow::Result;
use db::{
    review::{self, Manual, StatementAccounts},
    stops::{self, Open, Pick, Stop},
};
use import::{
    bank,
    tiantian::{self, Files},
};
use ledger::{accounts::Chart, corrected, daily, statements::bank::Bank};
use ledger_types::currency::Currency;
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
"333333333333" = "Assets:Cathay:Investment"
"444444444444" = "Assets:Cathay:Koko"

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
const KOKO: &str = "444444444444";

/// A line as (date, 說明, 提出, 存入, 餘額), oldest first.
type Line<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str);

/// The first download of each account: one spend on January 2.
const OPENING: &[Line] = &[("2026/01/02", "消費", "100", "", "900")];

/// A download of `account_no` through the day before `to`.
fn statement(account_no: &str, to: &str, lines: &[Line]) -> String {
    let columns = ["交易日期", "帳務日期", "說明", "提出", "存入", "餘額", "交易資訊", "備註"]
        .map(|c| format!("\"{c}\""))
        .join(",");
    let mut out = format!(
        "\u{feff}\"{account_no} 活存\"\r\n\"筆數\",\"(自 2025/12/01 至 \
         {to})\"\r\n\"幣別：TWD\"\r\n\"\r\n\"\r\n{columns}\r\n"
    );
    for (date, desc, out_, in_, bal) in lines.iter().rev() {
        out.push_str(&format!(
            "\"{date}\",\"{date}\",\"{desc}\",\"{out_}\",\"{in_}\",\"{bal}\",\"\",\"\"\r\n"
        ));
    }
    out
}

const EXPORT_HEADER: &str =
    "日期,類別,大類別,金額,幣別,成員,帳戶,標籤,備註,收支區分,上次更新,UUID\n";
const TRANSFER_HEADER: &str =
    "日期,從帳戶,轉出金額,幣別,到帳戶,轉入金額,幣別,標籤,備註,上次更新,UUID\n";

/// A 天天記帳 spend from 國泰.
fn spend(date: &str, amount: u32, id: &str) -> String {
    format!("{date},飲食,食食,{amount},TWD,自己,國泰,,午餐,支,{date},{id}\n")
}

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
        for account in [SAVINGS, INVESTMENT, KOKO] {
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
            paths.push(self.write(&name, &statement(account, to, lines))?);
        }
        let merged = Bank::Cathay.load_merged(&paths)?;
        bank::import_all(&self.pool, &self.chart, Bank::Cathay, &merged, &[]).await
    }

    async fn import_tiantian(&self, records: &[String]) -> Result<tiantian::Report> {
        let export: String =
            [EXPORT_HEADER].into_iter().chain(records.iter().map(String::as_str)).collect();
        let income_expense = self.write("收支.csv", &export)?;
        let transfers = self.write("轉帳.csv", TRANSFER_HEADER)?;
        let entries = daily::load_entries(&income_expense, &transfers)?;
        let frozen = corrected::frozen(self.dir.path().join("frozen.csv"))?;
        let files = Files { income_expense: &income_expense, transfers: &transfers };
        tiantian::import_all(&self.pool, &self.chart, &entries, &frozen, files).await
    }

    async fn transactions(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM transactions").fetch_one(&self.pool).await?)
    }

    /// The one stop open, which must be the only one.
    async fn the_stop(&self) -> Result<Open> {
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

    /// Each leg of transaction `id`, by account, with its origin.
    async fn legs(&self, id: i64) -> Result<Vec<(String, String, Option<String>)>> {
        Ok(sqlx::query_as(
            "SELECT a.path, p.amount, p.origin FROM postings p JOIN accounts a ON a.id = \
             p.account_id WHERE p.transaction_id = ? ORDER BY a.path",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn refs(&self, id: i64) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT external_ref FROM transaction_refs WHERE transaction_id = ? ORDER BY 1",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn events(&self, id: i64) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT kind FROM transaction_events WHERE transaction_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn unverified(&self) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(DISTINCT transaction_id) FROM postings WHERE tags LIKE '%unverified%'",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    /// A transfer between two statement accounts, entered by hand: each leg
    /// waits for its own statement.
    async fn transfer(&self, date: &str, from: &str, to: &str) -> Result<i64> {
        review::enter(&self.pool, &StatementAccounts::from(&self.chart), &Manual {
            date: date.parse()?,
            amount: dec!(300),
            currency: Currency::TWD,
            account: from.into(),
            category: None,
            counter: Some(to.into()),
            note: Some("轉帳".into()),
        })
        .await
    }
}

/// The 國泰 pool books a spend on 活存, but the bank shows it on 投資: the
/// statements vouch for its day and no 活存 line is it. The queue offers the
/// 投資 line, a person picks it, and the record relabels that line.
#[tokio::test]
async fn a_record_on_the_wrong_sibling_is_placed_on_the_line_picked() -> Result<()> {
    let f = Fixture::new().await?;
    let savings: &[Line] = &[OPENING[0], ("2026/01/25", "消費", "50", "", "850")];
    let investment: &[Line] = &[OPENING[0], ("2026/01/20", "消費", "200", "", "700")];
    f.import_bank(&[(SAVINGS, "2026/02/01", savings), (INVESTMENT, "2026/02/01", investment)])
        .await?;
    let line = f.line("Assets:Cathay:Investment", "2026-01-20").await?;
    let before = f.transactions().await?;

    let records = [spend("20260120", 200, "U1")];
    let err = f.import_tiantian(&records).await.expect_err("no 活存 line is U1");
    assert!(format!("{err:#}").contains("U1"), "{err:#}");
    assert_eq!(f.transactions().await?, before);
    let stop = f.the_stop().await?;
    let Stop::RecordUnexplained { record, through, lines, accounts } = &stop.stop else {
        panic!("{stop:?}")
    };
    assert_eq!(
        (record.account.as_str(), record.amount, through.to_string()),
        ("Assets:Cathay:Savings", dec!(-200), "2026-01-31".into())
    );
    assert_eq!(lines, &[line]);
    // Koko's statement does not reach the 20th: the record may be its.
    assert_eq!(accounts, &["Assets:Cathay:Koko"]);

    stops::choose(&f.pool, stop.id, &Pick::Line(line)).await?;
    assert_eq!(f.events(line).await?, ["paired"]);
    let report = f.import_tiantian(&records).await?;
    assert_eq!(report.counts.paired, 1, "{report}");
    assert!(report.check.ok(), "{report}");
    assert_eq!(f.transactions().await?, before);
    assert_eq!(f.legs(line).await?, [
        ("Assets:Cathay:Investment".into(), "-200".into(), None),
        ("Expenses:Food".into(), "200".into(), Some("tiantian".into())),
    ]);
    assert_eq!(f.refs(line).await?, ["tiantian:U1"]);
    assert!(stops::open(&f.pool).await?.is_empty());
    Ok(())
}

/// No line of its amount anywhere, but a sibling's statement does not reach
/// its day yet: placed there, it waits for that statement, which verifies it.
#[tokio::test]
async fn a_record_placed_on_a_sibling_waits_for_its_statement() -> Result<()> {
    let f = Fixture::new().await?;
    let savings: &[Line] = &[OPENING[0], ("2026/01/25", "消費", "50", "", "850")];
    f.import_bank(&[(SAVINGS, "2026/02/01", savings)]).await?;

    let records = [spend("20260120", 200, "U1")];
    f.import_tiantian(&records).await.expect_err("活存 shows no 200");
    let stop = f.the_stop().await?;
    let Stop::RecordUnexplained { lines, accounts, .. } = &stop.stop else { panic!("{stop:?}") };
    assert!(lines.is_empty());
    assert_eq!(accounts, &["Assets:Cathay:Investment", "Assets:Cathay:Koko"]);
    let refused = stops::choose(&f.pool, stop.id, &Pick::Account("Assets:Cash".into())).await;
    assert!(refused.is_err(), "only a sibling is offered");

    stops::choose(&f.pool, stop.id, &Pick::Account("Assets:Cathay:Koko".into())).await?;
    let report = f.import_tiantian(&records).await?;
    assert_eq!(report.counts.unverified, 1, "{report}");
    assert_eq!(f.unverified().await?, 1);

    let koko: &[Line] = &[OPENING[0], ("2026/01/20", "消費", "200", "", "700")];
    let bank = f.import_bank(&[(KOKO, "2026/02/01", koko)]).await?;
    assert_eq!((bank.counts.verified, bank.inserted), (1, 0), "{bank}");
    assert_eq!(f.unverified().await?, 0);
    Ok(())
}

/// Two lines of the record's amount within the window: nothing chooses until
/// a person does, and the other line stays uncategorised.
#[tokio::test]
async fn an_ambiguous_record_pairs_with_the_line_picked() -> Result<()> {
    let f = Fixture::new().await?;
    let savings: &[Line] = &[
        OPENING[0],
        ("2026/01/18", "消費", "200", "", "700"),
        ("2026/01/20", "消費", "200", "", "500"),
    ];
    f.import_bank(&[(SAVINGS, "2026/02/01", savings)]).await?;
    let (first, second) = (
        f.line("Assets:Cathay:Savings", "2026-01-18").await?,
        f.line("Assets:Cathay:Savings", "2026-01-20").await?,
    );

    let records = [spend("20260120", 200, "U1")];
    f.import_tiantian(&records).await.expect_err("two lines of 200");
    let stop = f.the_stop().await?;
    let Stop::RecordAmbiguous { lines, .. } = &stop.stop else { panic!("{stop:?}") };
    assert_eq!(lines, &[first, second]);

    stops::choose(&f.pool, stop.id, &Pick::Line(second)).await?;
    let report = f.import_tiantian(&records).await?;
    assert_eq!(report.counts.paired, 1, "{report}");
    assert_eq!(f.refs(second).await?, ["tiantian:U1"]);
    assert!(f.legs(first).await?.iter().any(|(a, ..)| a == "Expenses:Uncategorized"));
    Ok(())
}

/// A line a person already categorised is still the record's line: picked,
/// it keeps their category and only learns the record's ref.
#[tokio::test]
async fn a_line_already_categorised_keeps_its_category_when_picked() -> Result<()> {
    let f = Fixture::new().await?;
    let savings: &[Line] = &[OPENING[0], ("2026/01/20", "消費", "200", "", "700")];
    f.import_bank(&[(SAVINGS, "2026/02/01", savings)]).await?;
    let line = f.line("Assets:Cathay:Savings", "2026-01-20").await?;
    // The category a person gives it is one the ledger already has.
    let lunch = "20260105,飲食,食食,80,TWD,自己,現金,,,支,x,U0\n".to_string();
    f.import_tiantian(std::slice::from_ref(&lunch)).await?;
    let legs = f.legs(line).await?;
    let edit = review::Edit {
        narration: None,
        legs: sqlx::query_as::<_, (i64, String, String)>(
            "SELECT p.id, a.path, p.amount FROM postings p JOIN accounts a ON a.id = p.account_id \
             WHERE p.transaction_id = ?",
        )
        .bind(line)
        .fetch_all(&f.pool)
        .await?
        .into_iter()
        .map(|(id, path, amount)| review::LegEdit {
            posting_id: Some(id),
            account: match path.as_str() {
                "Expenses:Uncategorized" => "Expenses:Food".into(),
                _ => path,
            },
            amount: amount.parse().unwrap(),
            currency: Currency::TWD,
        })
        .collect(),
        confirm: true,
    };
    review::edit(&f.pool, &StatementAccounts::from(&f.chart), line, &edit).await?;
    assert_ne!(f.legs(line).await?, legs);

    let records = [lunch, spend("20260120", 200, "U1")];
    f.import_tiantian(&records).await.expect_err("no uncategorised line is U1");
    let stop = f.the_stop().await?;
    stops::choose(&f.pool, stop.id, &Pick::Line(line)).await?;
    f.import_tiantian(&records).await?;
    assert_eq!(f.legs(line).await?, [
        ("Assets:Cathay:Savings".into(), "-200".into(), None),
        ("Expenses:Food".into(), "200".into(), Some("manual".into())),
    ]);
    assert_eq!(f.refs(line).await?, ["tiantian:U1"]);
    Ok(())
}

/// Recorded before the statements reached it, the spend waits on 活存; the
/// 活存 statement passes it by and the 投資 one shows it. The import names
/// 投資, the record moves there, and the next import verifies it.
#[tokio::test]
async fn an_unverified_record_moves_to_the_sibling_that_shows_it() -> Result<()> {
    let f = Fixture::new().await?;
    f.import_tiantian(&[spend("20260120", 200, "U1")]).await?;
    let record: i64 =
        sqlx::query_scalar("SELECT id FROM transactions WHERE external_ref = 'tiantian:U1'")
            .fetch_one(&f.pool)
            .await?;
    let before = f.transactions().await?;

    let savings: &[Line] = &[OPENING[0], ("2026/01/25", "消費", "50", "", "850")];
    let investment: &[Line] = &[OPENING[0], ("2026/01/20", "消費", "200", "", "700")];
    let downloads = [(SAVINGS, "2026/02/01", savings), (INVESTMENT, "2026/02/01", investment)];
    let err = f.import_bank(&downloads).await.expect_err("活存 never shows U1");
    assert!(format!("{err:#}").contains("not on the statement"), "{err:#}");
    assert_eq!(f.transactions().await?, before);
    let stop = f.the_stop().await?;
    let Stop::NotOnStatement { transaction_id, siblings, .. } = &stop.stop else {
        panic!("{stop:?}")
    };
    assert_eq!(
        (*transaction_id, siblings.as_slice()),
        (record, &["Assets:Cathay:Investment".to_string()][..])
    );

    let statements = StatementAccounts::from(&f.chart);
    let refused = stops::move_leg(&f.pool, &statements, stop.id, "Assets:Cathay:Koko").await;
    assert!(refused.is_err(), "Koko shows no such line");
    stops::move_leg(&f.pool, &statements, stop.id, "Assets:Cathay:Investment").await?;
    assert_eq!(f.events(record).await?, ["edited"]);

    let report = f.import_bank(&downloads).await?;
    assert_eq!(report.counts.verified, 1, "{report}");
    assert!(report.check.ok(), "{report}");
    assert_eq!(f.unverified().await?, 0);
    assert_eq!(f.legs(record).await?[0].0, "Assets:Cathay:Investment");
    assert!(stops::open(&f.pool).await?.is_empty());
    Ok(())
}

/// A transfer to Koko in 活存's last days that its statement does not show:
/// the bank may book it on the next one, but moving the record would move
/// Koko's leg too. Split, the 活存 half waits alone.
#[tokio::test]
async fn a_shared_record_left_to_the_next_statement_is_split_first() -> Result<()> {
    let f = Fixture::new().await?;
    let record = f.transfer("2026-01-28", "Assets:Cathay:Savings", "Assets:Cathay:Koko").await?;

    let savings: &[Line] = &[OPENING[0], ("2026/01/25", "消費", "50", "", "850")];
    let downloads = [(SAVINGS, "2026/02/01", savings)];
    let err = f.import_bank(&downloads).await.expect_err("Koko's leg would move");
    assert!(format!("{err:#}").contains("Assets:Cathay:Koko"), "{err:#}");
    let stop = f.the_stop().await?;
    assert!(
        matches!(stop.stop, Stop::SharedDeferral { transaction_id, .. } if transaction_id == record)
    );

    let detached = stops::detach(&f.pool, stop.id).await?;
    assert_eq!(f.legs(record).await?.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), [
        "Assets:Cathay:Clearing",
        "Assets:Cathay:Savings"
    ]);
    assert_eq!(f.legs(detached).await?.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), [
        "Assets:Cathay:Clearing",
        "Assets:Cathay:Koko"
    ]);
    assert_eq!(
        (f.events(record).await?, f.events(detached).await?),
        (vec!["entered".to_string(), "edited".to_string()], vec!["detached".to_string()])
    );

    let report = f.import_bank(&downloads).await?;
    assert_eq!(report.counts.deferred, 1, "{report}");
    assert!(report.check.ok(), "{report}");
    assert!(stops::open(&f.pool).await?.is_empty());
    Ok(())
}

/// Each bank booked the transfer on its own day: split, each half takes its
/// own line's date, and the clearing account holds it in between.
#[tokio::test]
async fn a_record_two_lines_date_differently_is_split_into_one_per_bank() -> Result<()> {
    let f = Fixture::new().await?;
    let record = f.transfer("2026-01-20", "Assets:Cathay:Savings", "Assets:Cathay:Koko").await?;

    let savings: &[Line] = &[OPENING[0], ("2026/01/20", "轉出", "300", "", "600")];
    let koko: &[Line] = &[OPENING[0], ("2026/01/21", "轉入", "", "300", "1200")];
    let downloads = [(SAVINGS, "2026/02/01", savings), (KOKO, "2026/02/01", koko)];
    let err = f.import_bank(&downloads).await.expect_err("two days");
    assert!(format!("{err:#}").contains("split it"), "{err:#}");
    let stop = f.the_stop().await?;
    assert!(matches!(&stop.stop, Stop::TwoDates { dates, .. } if dates.len() == 2));

    let detached = stops::detach(&f.pool, stop.id).await?;
    let report = f.import_bank(&downloads).await?;
    assert_eq!(report.counts.verified, 2, "{report}");
    assert!(report.check.ok(), "{report}");
    let date = |id: i64| {
        sqlx::query_scalar::<_, String>("SELECT date FROM transactions WHERE id = ?")
            .bind(id)
            .fetch_one(&f.pool)
    };
    assert_eq!(
        (date(record).await?, date(detached).await?),
        ("2026-01-20".to_string(), "2026-01-21".to_string())
    );
    assert_eq!(f.unverified().await?, 0);
    Ok(())
}
