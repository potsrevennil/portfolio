//! `import_stop`: why an import stopped, typed so the review queue can show
//! each and settle it before the import runs again. The queue has a view for
//! every [`Stop`], so a new way to stop needs one too.

use std::{collections::HashMap, fmt};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger::labels::Labels;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use crate::{
    events::{self, Change, Detachment, EventKind, RecordPairing},
    import::{ensure_account, Origin},
    pairing::{self, Ambiguity},
    review::{self, Edit, LegEdit, StatementAccounts},
};

/// A 天天記帳 record an import could not place, as the export has it: it is
/// not in the ledger yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// `tiantian:<uuid>`.
    pub external_ref: String,
    pub date: NaiveDate,
    /// Where its statement leg books.
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    /// Where its other legs book: its category, or the account it moves to.
    pub counter: Vec<String>,
    /// Its note.
    pub narration: String,
}

/// Why an import stopped, over what the ledger holds rather than the files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stop {
    /// A statement line fitting several unverified records, or whose one
    /// record fits another line too. Kept in `verification_choice`.
    LineAmbiguous(Ambiguity),
    /// A record fitting several uncategorised lines, or whose one line fits
    /// another record too.
    RecordAmbiguous { record: Record, lines: Vec<i64> },
    /// A record on a day the statements vouch for, with no uncategorised line
    /// for it. `lines` are the statement lines of its amount it might be all
    /// the same (categorised already, or on a sibling account); `accounts`
    /// the siblings whose statements do not reach its day yet.
    RecordUnexplained { record: Record, through: NaiveDate, lines: Vec<i64>, accounts: Vec<String> },
    /// An unverified record the statement passes without a line for it.
    /// `siblings` are the accounts sharing its app account where a line of
    /// this import fits it.
    NotOnStatement {
        transaction_id: i64,
        account: String,
        currency: Currency,
        date: NaiveDate,
        amount: Decimal,
        through: NaiveDate,
        siblings: Vec<String>,
    },
    /// An unverified record from the statement's last days that the bank may
    /// book after it, with a leg on `other`, another statement account:
    /// moving it to the next statement would move that leg too.
    SharedDeferral {
        transaction_id: i64,
        account: String,
        currency: Currency,
        date: NaiveDate,
        amount: Decimal,
        other: String,
        clearing: String,
    },
    /// Two statements' lines verify one record, each on its own day.
    TwoDates { transaction_id: i64, dates: Vec<(String, NaiveDate)>, clearing: String },
    /// An account with more than one opening.
    SeveralOpenings { account: String, currency: Currency },
    /// The ledger plus the import would not end a statement day on its
    /// balance: a download starts mid-day, overlaps unevenly, or is missing.
    Unchained {
        account: String,
        currency: Currency,
        day: NaiveDate,
        ledger: Decimal,
        statement: Decimal,
    },
}

impl Stop {
    /// The `import_stop.kind` word.
    fn kind(&self) -> &'static str {
        match self {
            Stop::LineAmbiguous(_) => "line_ambiguous",
            Stop::RecordAmbiguous { .. } => "record_ambiguous",
            Stop::RecordUnexplained { .. } => "record_unexplained",
            Stop::NotOnStatement { .. } => "not_on_statement",
            Stop::SharedDeferral { .. } => "shared_deferral",
            Stop::TwoDates { .. } => "two_dates",
            Stop::SeveralOpenings { .. } => "several_openings",
            Stop::Unchained { .. } => "unchained",
        }
    }

    /// What it is about, so a run meeting it again updates its row.
    fn subject(&self) -> String {
        match self {
            Stop::LineAmbiguous(a) => a.statement_ref.clone(),
            Stop::RecordAmbiguous { record, .. } | Stop::RecordUnexplained { record, .. } => {
                record.external_ref.clone()
            }
            Stop::NotOnStatement { transaction_id, .. }
            | Stop::SharedDeferral { transaction_id, .. }
            | Stop::TwoDates { transaction_id, .. } => transaction_id.to_string(),
            Stop::SeveralOpenings { account, currency }
            | Stop::Unchained { account, currency, .. } => {
                format!("{account} {currency}")
            }
        }
    }
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::LineAmbiguous(a) => write!(
                f,
                "{} {}: the line {} {} and the unverified records it fits ({}) pair ambiguously; \
                 pick one in 待配對",
                a.account,
                a.currency,
                a.date,
                a.amount,
                a.candidates.len()
            ),
            Stop::RecordAmbiguous { record: r, lines } => write!(
                f,
                "{} {} {} on {}: fits {} bank lines; pick one in the review queue",
                r.external_ref,
                r.date,
                r.amount,
                r.account,
                lines.len()
            ),
            Stop::RecordUnexplained { record: r, through, .. } => write!(
                f,
                "{} {} {} on {}: the statements through {through} show no line for it",
                r.external_ref, r.date, r.amount, r.account
            ),
            Stop::NotOnStatement {
                transaction_id,
                account,
                currency,
                date,
                amount,
                through,
                ..
            } => {
                write!(
                    f,
                    "{account} {currency}: the unverified record {transaction_id} of {amount} on \
                     {date} is not on the statement through {through}"
                )
            }
            Stop::SharedDeferral { account, currency, date, amount, other, .. } => write!(
                f,
                "{account} {currency}: the record of {amount} on {date} is also {other}'s, and \
                 this statement does not show it. Import it together with the next statement, \
                 which may, or split it in the review queue"
            ),
            Stop::TwoDates { transaction_id, dates, .. } => {
                let days: Vec<String> = dates.iter().map(|(a, d)| format!("{d} on {a}")).collect();
                write!(
                    f,
                    "the record {transaction_id} was booked {}; split it in the review queue",
                    days.join(" and ")
                )
            }
            Stop::SeveralOpenings { account, currency } => {
                write!(f, "{account} {currency} has more than one opening")
            }
            Stop::Unchained { account, currency, day, ledger, statement } => write!(
                f,
                "{account} {currency}: the ledger would end {day} at {ledger} but the statement \
                 says {statement}. A download starts mid-day, overlaps one already imported \
                 unevenly, or one is missing"
            ),
        }
    }
}

/// An import that stopped: it wrote nothing, and these say why.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub Vec<Stop>);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the import stopped and nothing was imported; settle these in 待確認, then import \
             again:"
        )?;
        for stop in &self.0 {
            write!(f, "\n  {stop}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Refused {}

/// What a person chose for a record the import could not place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    /// The statement line that is this record.
    Line(i64),
    /// The sibling account it books on instead, waiting for its statement.
    Account(String),
}

/// A stop no later run of its importer has cleared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub id: i64,
    pub stop: Stop,
    pub pick: Option<Pick>,
    /// Settled by a write; the next import is what is left.
    pub resolved: bool,
}

#[derive(sqlx::FromRow)]
struct OpenRow {
    id: i64,
    problem: String,
    pick: Option<String>,
    resolved: bool,
}

impl TryFrom<OpenRow> for Open {
    type Error = anyhow::Error;

    fn try_from(r: OpenRow) -> Result<Self> {
        Ok(Open {
            id: r.id,
            stop: serde_json::from_str(&r.problem).context("an import stop")?,
            pick: r.pick.map(|p| serde_json::from_str(&p)).transpose().context("a pick")?,
            resolved: r.resolved,
        })
    }
}

const OPEN: &str = "SELECT id, problem, pick, resolved_at IS NOT NULL AS resolved FROM import_stop
    WHERE cleared_at IS NULL";

/// Records what a refused run of `source` met, in place of what its last run
/// left open. A pick stays unless its own stop is met again, when it no
/// longer fits and a person picks again.
pub async fn record(db: &mut SqliteConnection, source: &str, stops: &[Stop]) -> Result<()> {
    sqlx::query(
        "UPDATE import_stop SET cleared_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE source = ? AND cleared_at IS NULL AND pick IS NULL",
    )
    .bind(source)
    .execute(&mut *db)
    .await?;
    let mut lines = Vec::new();
    for stop in stops {
        if let Stop::LineAmbiguous(a) = stop {
            lines.push(a.clone());
            continue;
        }
        sqlx::query(
            "INSERT INTO import_stop (source, kind, subject, problem) VALUES (?, ?, ?, ?)
             ON CONFLICT (kind, subject) DO UPDATE SET source = excluded.source,
                problem = excluded.problem, pick = NULL, resolved_at = NULL, cleared_at = NULL",
        )
        .bind(source)
        .bind(stop.kind())
        .bind(stop.subject())
        .bind(serde_json::to_string(stop)?)
        .execute(&mut *db)
        .await
        .with_context(|| format!("recording the stop {stop}"))?;
    }
    pairing::record(db, &lines).await
}

/// Ends an import run in `tx`: one that succeeded commits, clearing what
/// the last refusal left open; one that stopped rolls back, and what stopped
/// it is kept for the queue.
pub async fn finish<T>(
    pool: &SqlitePool,
    source: &str,
    mut tx: Transaction<'static, Sqlite>,
    outcome: Result<T>,
) -> Result<T> {
    match outcome {
        Ok(report) => {
            clear(&mut tx, source).await?;
            tx.commit().await?;
            Ok(report)
        }
        Err(e) => {
            tx.rollback().await?;
            if let Some(Refused(found)) = e.downcast_ref() {
                let mut kept = pool.begin().await?;
                record(&mut kept, source, found).await?;
                kept.commit().await?;
            }
            Err(e)
        }
    }
}

/// Clears what `source` left open: a run that succeeds met none of it.
pub async fn clear(db: &mut SqliteConnection, source: &str) -> Result<()> {
    sqlx::query(
        "UPDATE import_stop SET cleared_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE source = ? AND cleared_at IS NULL",
    )
    .bind(source)
    .execute(db)
    .await?;
    Ok(())
}

/// Every stop still open, oldest first.
pub async fn open(pool: &SqlitePool) -> Result<Vec<Open>> {
    let rows: Vec<OpenRow> = sqlx::query_as(&format!("{OPEN} ORDER BY id"))
        .fetch_all(pool)
        .await
        .context("reading import stops")?;
    rows.into_iter().map(Open::try_from).collect()
}

/// The picks `source`'s next run places its records by, by record ref.
pub async fn picks(db: &mut SqliteConnection, source: &str) -> Result<HashMap<String, Pick>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT subject, pick FROM import_stop
         WHERE source = ? AND cleared_at IS NULL AND pick IS NOT NULL",
    )
    .bind(source)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(subject, pick)| Ok((subject, serde_json::from_str(&pick).context("a pick")?)))
        .collect()
}

async fn open_one(db: &mut SqliteConnection, id: i64) -> Result<Open> {
    let row: OpenRow = sqlx::query_as(&format!("{OPEN} AND id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?
        .with_context(|| format!("揣無這个匯入問題 {id}"))?;
    Open::try_from(row)
}

async fn resolve(db: &mut SqliteConnection, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE import_stop SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Picks where the record of stop `id` belongs: one of the lines offered, or
/// one of the accounts. A line picked for another open record cannot be
/// picked again: one line is one record.
pub async fn choose(pool: &SqlitePool, id: i64, pick: &Pick) -> Result<()> {
    let mut db = pool.begin().await?;
    let open = open_one(&mut db, id).await?;
    let (record, lines, accounts) = match &open.stop {
        Stop::RecordAmbiguous { record, lines } => (record, lines.as_slice(), &[][..]),
        Stop::RecordUnexplained { record, lines, accounts, .. } => {
            (record, lines.as_slice(), accounts.as_slice())
        }
        _ => bail!("這筆免揀"),
    };
    match pick {
        Pick::Line(line) => {
            let deleted: Option<String> =
                sqlx::query_scalar("SELECT deleted_at FROM transactions WHERE id = ?")
                    .bind(line)
                    .fetch_optional(&mut *db)
                    .await?
                    .flatten();
            if !lines.contains(line) || deleted.is_some() {
                bail!("這筆無佇會使配的明細內底");
            }
            let taken: Option<String> = sqlx::query_scalar(
                "SELECT subject FROM import_stop
                 WHERE cleared_at IS NULL AND json_extract(pick, '$.line') = ? AND id <> ?",
            )
            .bind(line)
            .bind(id)
            .fetch_optional(&mut *db)
            .await?;
            if taken.is_some() {
                bail!("這筆明細已經配予另外一筆紀錄");
            }
            let pairing = RecordPairing {
                record_ref: record.external_ref.clone(),
                date: record.date,
                amount: record.amount,
            };
            events::record(&mut db, *line, EventKind::Paired, &pairing).await?;
        }
        Pick::Account(account) if !accounts.contains(account) => bail!("袂使記佇這个帳戶"),
        Pick::Account(_) => {}
    }
    sqlx::query(
        "UPDATE import_stop SET pick = ?, resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?",
    )
    .bind(serde_json::to_string(pick)?)
    .bind(id)
    .execute(&mut *db)
    .await?;
    db.commit().await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct LegRow {
    id: i64,
    path: String,
    amount: String,
    currency: String,
}

async fn legs(db: &mut SqliteConnection, transaction_id: i64) -> Result<Vec<LegRow>> {
    Ok(sqlx::query_as(
        "SELECT p.id, a.path, p.amount, p.currency FROM postings p
         JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(transaction_id)
    .fetch_all(db)
    .await?)
}

/// Splits the record of stop `id` in two through the clearing account, so
/// each bank's leg is a record of its own and can take its own date: the
/// legs on the other statement account move to a new transaction, and a
/// clearing leg on each side holds what is in transit. Returns the new
/// transaction's id.
pub async fn detach(pool: &SqlitePool, id: i64) -> Result<i64> {
    let mut db = pool.begin().await?;
    let before_check = review::baseline(&mut db).await?;
    let (transaction_id, moving, clearing) = match open_one(&mut db, id).await?.stop {
        Stop::SharedDeferral { transaction_id, other, clearing, .. } => {
            (transaction_id, vec![other], clearing)
        }
        Stop::TwoDates { transaction_id, dates, clearing } => {
            (transaction_id, dates.into_iter().skip(1).map(|(a, _)| a).collect(), clearing)
        }
        _ => bail!("這筆袂使拆"),
    };
    if clearing.is_empty() {
        bail!("mapping.toml 的 [institution] 無設 clearing（在途帳戶）");
    }
    review::live(&mut db, transaction_id).await?;
    let before = events::snapshot(&mut db, transaction_id).await?;
    let (moved, kept): (Vec<LegRow>, Vec<LegRow>) =
        legs(&mut db, transaction_id).await?.into_iter().partition(|l| moving.contains(&l.path));
    if moved.is_empty() || kept.is_empty() {
        bail!("這筆已經毋是兩个帳戶之間的轉帳");
    }

    let detached = sqlx::query(
        "INSERT INTO transactions (date, payee, narration, source, reviewed)
         SELECT date, payee, narration, source, reviewed FROM transactions WHERE id = ?",
    )
    .bind(transaction_id)
    .execute(&mut *db)
    .await?
    .last_insert_rowid();
    let mut in_transit: HashMap<Currency, Decimal> = HashMap::new();
    for leg in &moved {
        sqlx::query("UPDATE postings SET transaction_id = ? WHERE id = ?")
            .bind(detached)
            .bind(leg.id)
            .execute(&mut *db)
            .await?;
        let amount: Decimal = leg.amount.parse().context("posting amount")?;
        let currency: Currency = leg.currency.parse().context("posting currency")?;
        *in_transit.entry(currency).or_default() += amount;
    }
    let clearing_id = ensure_account(&mut db, &Labels::default(), &clearing).await?;
    for (currency, amount) in &in_transit {
        for (txn, amount) in [(transaction_id, *amount), (detached, -*amount)] {
            sqlx::query(
                "INSERT INTO postings (transaction_id, account_id, amount, currency, origin)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(txn)
            .bind(clearing_id)
            .bind(amount.to_string())
            .bind(currency.to_string())
            .bind(Origin::Manual.to_string())
            .execute(&mut *db)
            .await?;
        }
    }

    let after = events::snapshot(&mut db, transaction_id).await?;
    events::record(&mut db, transaction_id, EventKind::Edited, &Change {
        before: Some(before),
        after,
    })
    .await?;
    let after = events::snapshot(&mut db, detached).await?;
    let detachment = Detachment { from: transaction_id, before: None, after };
    events::record(&mut db, detached, EventKind::Detached, &detachment).await?;
    resolve(&mut db, id).await?;
    review::gated(db, before_check).await?;
    Ok(detached)
}

/// Moves the leg of stop `id`'s record to `to`, a sibling account whose
/// statement shows it: an edit, held to the balances like any other.
pub async fn move_leg(
    pool: &SqlitePool,
    statements: &StatementAccounts,
    id: i64,
    to: &str,
) -> Result<()> {
    let mut db = pool.begin().await?;
    let before_check = review::baseline(&mut db).await?;
    let (transaction_id, account, amount, currency) =
        match open_one(&mut db, id).await?.stop {
            Stop::NotOnStatement {
                transaction_id, account, amount, currency, siblings, ..
            } if siblings.iter().any(|s| s == to) => (transaction_id, account, amount, currency),
            Stop::NotOnStatement { .. } => bail!("袂使改記佇這个帳戶"),
            _ => bail!("這筆袂使改帳戶"),
        };
    let narration: Option<String> =
        sqlx::query_scalar("SELECT narration FROM transactions WHERE id = ?")
            .bind(transaction_id)
            .fetch_one(&mut *db)
            .await?;
    let mut found = false;
    let mut edit = Edit { narration, legs: Vec::new(), confirm: false };
    for leg in legs(&mut db, transaction_id).await? {
        let (amount_here, currency_here): (Decimal, Currency) =
            (leg.amount.parse()?, leg.currency.parse()?);
        let this =
            !found && leg.path == account && (amount_here, currency_here) == (amount, currency);
        found |= this;
        edit.legs.push(LegEdit {
            posting_id: Some(leg.id),
            account: match this {
                true => to.to_string(),
                false => leg.path,
            },
            amount: amount_here,
            currency: currency_here,
        });
    }
    if !found {
        bail!("這筆已經改過，無佇原來的帳戶");
    }
    review::edit_in(&mut db, statements, transaction_id, &edit).await?;
    resolve(&mut db, id).await?;
    review::gated(db, before_check).await
}
