//! Builds the coverage page: for each configured source, how far outside
//! figures vouch for it, how far its records reach, and what lies between.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::Result;
use chrono::{Days, NaiveDate};
use db::{
    coverage::{self, Figure, Record},
    query::{in_subtree, AccountType, LedgerData},
    SqlitePool,
};
use ledger::{
    coverage::{Cadence, Source, Sources, Standing},
    manifest::{Manifest, RawFile},
};

use crate::{
    coverage::{Coverage, Files, Gap, Missing, SourceRow, Status, Tail, Unsourced},
    model::JournalQuery,
};

/// What the page reads besides the database.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub sources: Sources,
    /// `raw/manifest.csv`; a ledger without one lists no files.
    pub manifest: PathBuf,
}

pub async fn load(pool: &SqlitePool, config: &Config, today: NaiveDate) -> Result<Coverage> {
    let mut conn = pool.acquire().await?;
    let figures = coverage::figures(&mut conn).await?;
    let manifest = match config.manifest.exists() {
        true => Manifest::load(&config.manifest)?,
        false => Manifest::default(),
    };
    let data = LedgerData::load(pool).await?;
    let labels = data.labels();

    let mut sources = Vec::new();
    for source in &config.sources.0 {
        let figures: Vec<&Figure> =
            figures.iter().filter(|f| in_subtree(&f.account, &source.account)).collect();
        let broker = coverage::broker_records(&mut conn, &source.account).await?;
        // A broker's own records are its sub-ledger; the postings on its
        // account are only the money sent in.
        let held_by_broker = !broker.is_empty() || figures.iter().any(|f| f.kind != "balance");
        let held = Held {
            records: match held_by_broker {
                true => broker,
                false => coverage::ledger_records(&mut conn, &source.account).await?,
            },
            journal: !held_by_broker,
            files: source.raw.as_deref().map(|r| manifest.under(r)).unwrap_or_default(),
            figures,
        };
        let label = source.label.clone().unwrap_or_else(|| label(&labels, &source.account));
        sources.push(row(source, label, &labels, &held, today));
    }

    let roots: Vec<&str> = config.sources.0.iter().map(|s| s.account.as_str()).collect();
    let last = data.last_posting_dates();
    let mut unsourced: BTreeMap<String, Unsourced> = BTreeMap::new();
    for b in data.balances_as_of(NaiveDate::MAX) {
        let shown = matches!(b.account_type, AccountType::Asset | AccountType::Liability)
            && !b.closed
            && !b.amount.is_zero()
            && !roots.iter().any(|r| in_subtree(&b.path, r));
        if shown {
            let imported = last.get(&(b.account_id, b.currency)).copied();
            let entry = unsourced.entry(b.path.clone()).or_insert_with(|| Unsourced {
                label: label(&labels, &b.path),
                account: b.path.clone(),
                imported: None,
            });
            entry.imported = entry.imported.clone().max(imported.map(|d| d.to_string()));
        }
    }

    Ok(Coverage { today: today.to_string(), sources, unsourced: unsourced.into_values().collect() })
}

/// The account's labels from its group down (`Assets:Bank:Alpha:Savings` →
/// `銀行 甲銀行 活存`): the list has no tree to say which bank a 活存 is.
fn label(labels: &BTreeMap<&str, &str>, path: &str) -> String {
    let segments: Vec<&str> = path.split(':').collect();
    (2.min(segments.len())..=segments.len())
        .map(|depth| {
            let prefix = segments[..depth].join(":");
            labels.get(prefix.as_str()).map_or(segments[depth - 1], |l| l).to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How far figures vouch for one account and commodity of a source.
struct Vouched {
    /// The newest day a figure vouches for; `None` if none ever did.
    through: Option<NaiveDate>,
    /// Its newest figure was zero and nothing moved it since: nothing is left
    /// for a later figure to check.
    settled: bool,
}

/// What a source holds: the figures vouching for it and its own records.
struct Held<'a> {
    figures: Vec<&'a Figure>,
    records: Vec<Record>,
    /// The records are postings, which the journal lists.
    journal: bool,
    files: Vec<&'a RawFile>,
}

fn row(
    source: &Source,
    label: String,
    labels: &BTreeMap<&str, &str>,
    held: &Held<'_>,
    today: NaiveDate,
) -> SourceRow {
    let Held { figures, records, journal, files } = held;
    // Per account and commodity: a figure covers its account's subtree, and
    // a count of one wallet says nothing of the wallet beside it.
    let mut keys: BTreeSet<(&str, &str)> =
        records.iter().map(|r| (r.account.as_str(), r.commodity.as_str())).collect();
    keys.extend(figures.iter().map(|f| (f.account.as_str(), f.commodity.as_str())));
    let vouched: BTreeMap<(&str, &str), Vouched> = keys
        .into_iter()
        .map(|(account, commodity)| {
            let newest = figures
                .iter()
                .filter(|f| f.commodity == commodity && in_subtree(account, &f.account))
                .max_by_key(|f| f.as_of);
            let through = newest.map(|f| f.as_of);
            let moved = records.iter().any(|r| {
                r.commodity == commodity
                    && in_subtree(&r.account, account)
                    && through.is_none_or(|t| r.date > t)
            });
            let settled = newest.is_some_and(|f| f.figure.is_zero()) && !moved;
            ((account, commodity), Vouched { through, settled })
        })
        .collect();
    let open: Vec<(String, Option<NaiveDate>)> = vouched
        .iter()
        .filter(|(_, v)| !v.settled)
        .map(|((account, commodity), v)| {
            let name = match *account == source.account {
                true => commodity.to_string(),
                false => {
                    let leaf = account.rsplit(':').next().unwrap_or(account);
                    format!("{} {commodity}", labels.get(account).unwrap_or(&leaf))
                }
            };
            (name, v.through)
        })
        .collect();
    // The source is checked only as far as its least recently checked
    // commodity: a 外幣 account vouched in USD says nothing about its JPY.
    let checked = match open.is_empty() {
        true => vouched.values().filter_map(|v| v.through).max(),
        false => open.iter().map(|(_, t)| *t).min().flatten(),
    };
    let distinct: BTreeSet<Option<NaiveDate>> = open.iter().map(|(_, t)| *t).collect();
    let per_commodity = match distinct.len() > 1 {
        true => open
            .iter()
            .map(|(name, t)| match t {
                Some(t) => format!("{name} {t}"),
                None => format!("{name} 從未"),
            })
            .collect(),
        false => Vec::new(),
    };

    let tail_records: Vec<&Record> = records
        .iter()
        .filter(|r| {
            let through =
                vouched.get(&(r.account.as_str(), r.commodity.as_str())).and_then(|v| v.through);
            r.unverified || through.is_none_or(|t| r.date > t)
        })
        .collect();
    let tail = tail(&tail_records, journal.then_some(source.account.as_str()));

    let standing = Standing::of(source.cadence, source.grace, checked, tail.is_some(), today);
    let counted = matches!(source.cadence, Cadence::Count { .. });
    let status = match standing {
        Standing::Checked => Status::Checked,
        Standing::Unchecked => Status::Unchecked,
        Standing::Due if counted => Status::ToCount,
        Standing::Due => Status::ToDownload,
        Standing::Overdue => Status::Overdue,
    };
    let missed = source.cadence.missed(checked, today);
    let missing = matches!(standing, Standing::Due | Standing::Overdue).then(|| Missing {
        from: missed.and_then(|m| m.since).map(|d| d.to_string()),
        through: missed.map(|m| m.due.to_string()),
        count: counted,
        url: source.url.clone(),
    });

    SourceRow {
        label,
        account: source.account.clone(),
        cadence: source.cadence.to_string(),
        status,
        checked: checked.map(|d| d.to_string()),
        per_commodity,
        behind_days: checked.map(|c| (today - c).num_days()),
        imported: records.iter().map(|r| r.date).max().map(|d| d.to_string()),
        tail,
        gaps: gaps(figures),
        missing,
        // The newest download where the manifest dates them, else the last name.
        files: files.iter().max_by_key(|f| (f.downloaded, &f.raw_path)).map(|latest| Files {
            count: files.len(),
            latest: latest.raw_path.clone(),
            downloaded: files.iter().filter_map(|f| f.downloaded).max().map(|d| d.to_string()),
        }),
    }
}

fn tail(records: &[&Record], journal_account: Option<&str>) -> Option<Tail> {
    let dates = records.iter().map(|r| r.date);
    match (dates.clone().min(), dates.max()) {
        (Some(first), Some(last)) => {
            let link = journal_account.map(|account| {
                let query = JournalQuery {
                    from: Some(first.to_string()),
                    unverified: records.iter().all(|r| r.unverified),
                    ..JournalQuery::account(account)
                };
                format!("/journal{query}")
            });
            let count = records.iter().map(|r| r.id).collect::<BTreeSet<_>>().len();
            Some(Tail { count, first: first.to_string(), last: last.to_string(), link })
        }
        _ => None,
    }
}

/// Days no statement covers between two that do, per account and currency.
fn gaps(figures: &[&Figure]) -> Vec<Gap> {
    let mut periods: BTreeMap<(&str, &str), Vec<(NaiveDate, NaiveDate)>> = BTreeMap::new();
    for f in figures {
        if let Some(start) = f.period_start {
            periods.entry((&f.account, &f.commodity)).or_default().push((start, f.as_of));
        }
    }
    let mut gaps = Vec::new();
    for ((_, commodity), mut spans) in periods {
        spans.sort();
        let mut reached: Option<NaiveDate> = None;
        for (start, end) in spans {
            if let Some(reached) = reached.filter(|r| start > *r + Days::new(1)) {
                gaps.push(Gap {
                    commodity: commodity.to_string(),
                    from: (reached + Days::new(1)).to_string(),
                    to: (start - Days::new(1)).to_string(),
                });
            }
            reached = reached.max(Some(end));
        }
    }
    gaps
}
