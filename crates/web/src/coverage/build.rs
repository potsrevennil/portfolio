//! Builds the 對帳進度 page: for every asset and liability account, how far
//! outside figures vouch for it, how far its records reach, and what its
//! statement schedule asks for.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::{Datelike, Days, Months, NaiveDate};
use db::{
    assertions,
    coverage::{self, Figure, Gap, Position, Record, Schedule},
    SqliteConnection, SqlitePool,
};
use ledger::accounts::{in_subtree, SplitAccounts};
use ledger_types::{
    cadence::{Cadence, Missed, Standing, GRACE},
    currency::Currency,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};

use super::model::{
    oldest, AccountRow, Coverage, GapSpan, Holding, Instalment, Mark, SplitOpen, Tail, Tick,
};
use crate::{
    entries::{Chart, Unlisted},
    model::{Crumb, JournalQuery, Money},
};

pub async fn load(
    pool: &SqlitePool,
    today: NaiveDate,
    splits: &SplitAccounts,
    base: Currency,
) -> Result<Coverage> {
    // Only names are read here, and every account has one.
    let chart = Chart::load(pool, &Unlisted::default()).await?;
    let mut conn = pool.acquire().await?;
    let books = Books::read(&mut conn).await?;
    let gaps = coverage::gaps(&assertions::load(&mut conn).await?);
    let ledger = by_account(coverage::ledger_records(&mut conn).await?, |r| &r.account);
    let broker = by_account(coverage::broker_records(&mut conn).await?, |r| &r.account);
    let mut rows: Vec<AccountRow> = books
        .accounts()
        .iter()
        .map(|held| {
            let records = match held.journal {
                true => &ledger,
                false => &broker,
            };
            let records = records.get(held.account).map(Vec::as_slice).unwrap_or_default();
            row(held, records, &gaps, &chart.crumbs(held.account), splits, base, today)
        })
        .collect();
    // Instalments need no statement of their own, so they come last: the
    // page is for what needs fetching. The sort is stable, so the chart's
    // order holds otherwise.
    rows.sort_by_key(|r| r.instalment.is_some());

    Ok(Coverage { today: today.to_string(), months: months(&rows, today), accounts: rows })
}

/// How many accounts holding something await a figure, for the nav's badge
/// on every page.
pub async fn due(pool: &SqlitePool, today: NaiveDate) -> Result<u64> {
    let mut conn = pool.acquire().await?;
    let books = Books::read(&mut conn).await?;
    let accounts = books.accounts();
    let due = accounts.iter().filter(|h| !h.balances().is_empty() && h.standing(today).is_due());
    Ok(due.count() as u64)
}

/// Everything the page weighs an account against.
struct Books {
    figures: Vec<Figure>,
    ledger: BTreeMap<String, Vec<Position>>,
    broker: BTreeMap<String, Vec<Position>>,
    schedules: BTreeMap<String, Schedule>,
}

impl Books {
    async fn read(conn: &mut SqliteConnection) -> Result<Self> {
        Ok(Books {
            figures: coverage::figures(conn).await?,
            ledger: by_account(coverage::ledger_positions(conn).await?, |p| &p.account),
            broker: by_account(coverage::broker_positions(conn).await?, |p| &p.account),
            schedules: coverage::schedules(conn)
                .await?
                .into_iter()
                .map(|s| (s.account.clone(), s))
                .collect(),
        })
    }

    /// Every account with a record or a figure, in the chart's order.
    fn accounts(&self) -> Vec<Held<'_>> {
        let outside: Vec<&Figure> = self.figures.iter().filter(|f| f.source.is_outside()).collect();
        let accounts: BTreeSet<&str> = self
            .ledger
            .keys()
            .chain(self.broker.keys())
            .map(String::as_str)
            .chain(outside.iter().map(|f| f.account.as_str()))
            .collect();
        accounts
            .into_iter()
            .map(|account| {
                let figures: Vec<&Figure> =
                    outside.iter().copied().filter(|f| in_subtree(account, &f.account)).collect();
                // A broker's own records are its sub-ledger; the postings on
                // its account are only the money sent in.
                let held_by_broker =
                    self.broker.contains_key(account) || figures.iter().any(|f| f.held_to_broker);
                let ledger = self.ledger.get(account).map(Vec::as_slice).unwrap_or_default();
                let checked = match held_by_broker {
                    true => self.broker.get(account).map(Vec::as_slice).unwrap_or_default(),
                    false => ledger,
                };
                Held {
                    account,
                    figures,
                    ledger,
                    checked,
                    journal: !held_by_broker,
                    schedule: self.schedules.get(account),
                }
            })
            .collect()
    }
}

fn by_account<T>(rows: Vec<T>, account: fn(&T) -> &String) -> BTreeMap<String, Vec<T>> {
    let mut map: BTreeMap<String, Vec<T>> = BTreeMap::new();
    for row in rows {
        map.entry(account(&row).clone()).or_default().push(row);
    }
    map
}

/// One account's figures and records, and how it is scheduled.
struct Held<'a> {
    account: &'a str,
    figures: Vec<&'a Figure>,
    /// What moved its balance.
    ledger: &'a [Position],
    /// What its figures check: the ledger's, or a broker's own records.
    checked: &'a [Position],
    /// What its figures check are postings, which the journal lists.
    journal: bool,
    schedule: Option<&'a Schedule>,
}

/// How far figures vouch for one commodity of an account.
struct Vouched {
    /// The newest day a figure vouches for; `None` if none ever did.
    through: Option<NaiveDate>,
    /// Nothing moved it since its newest figure: a dormant currency, whose
    /// figure still holds, does not keep the account behind.
    settled: bool,
}

impl Held<'_> {
    /// What it holds now, per currency; empty when inactive.
    fn balances(&self) -> Vec<Holding> {
        let mut sums: BTreeMap<Currency, Decimal> = BTreeMap::new();
        for p in self.ledger {
            if let Ok(currency) = p.commodity.parse() {
                *sums.entry(currency).or_default() += p.amount;
            }
        }
        sums.into_iter()
            .filter(|(_, amount)| !amount.is_zero())
            .map(|(currency, amount)| Holding { currency, amount: amount.normalize().to_string() })
            .collect()
    }

    fn vouched(&self) -> BTreeMap<&str, Vouched> {
        let mut commodities: BTreeSet<&str> =
            self.checked.iter().map(|p| p.commodity.as_str()).collect();
        commodities.extend(self.figures.iter().map(|f| f.commodity.as_str()));
        commodities
            .into_iter()
            .map(|commodity| {
                let through =
                    self.figures.iter().filter(|f| f.commodity == commodity).map(|f| f.as_of).max();
                let moved = self
                    .checked
                    .iter()
                    .any(|p| p.commodity == commodity && through.is_none_or(|t| p.last > t));
                (commodity, Vouched { through, settled: !moved })
            })
            .collect()
    }

    fn cadence(&self) -> Cadence { self.schedule.map_or(Cadence::Never, |s| s.cadence) }

    fn grace(&self) -> u32 { self.schedule.map_or(GRACE, |s| s.grace) }

    /// What its cadence counts from: a loan is kept current by its monthly
    /// payment record, not a figure.
    fn reference(&self, checked: Option<NaiveDate>) -> Option<NaiveDate> {
        match self.cadence() {
            Cadence::Payment { .. } => self.checked.iter().map(|p| p.last).max(),
            _ => checked,
        }
    }

    fn standing(&self, today: NaiveDate) -> Standing {
        let checked = through(&self.vouched());
        self.cadence().standing(self.reference(checked), self.grace(), today)
    }
}

/// Checked only as far as its least recently checked currency: a 外幣 account
/// vouched in USD says nothing about its JPY.
fn through(vouched: &BTreeMap<&str, Vouched>) -> Option<NaiveDate> {
    let mut open = vouched.values().filter(|v| !v.settled).map(|v| v.through).peekable();
    match open.peek() {
        Some(_) => open.min().flatten(),
        None => vouched.values().filter_map(|v| v.through).max(),
    }
}

fn row(
    held: &Held<'_>,
    records: &[Record],
    gaps: &[Gap],
    crumbs: &[Crumb],
    splits: &SplitAccounts,
    base: Currency,
    today: NaiveDate,
) -> AccountRow {
    let Held { account, figures, schedule, .. } = held;
    let vouched = held.vouched();
    let checked = through(&vouched);
    let distinct: BTreeSet<Option<NaiveDate>> =
        vouched.values().filter(|v| !v.settled).map(|v| v.through).collect();
    let per_commodity = match distinct.len() > 1 {
        true => vouched
            .iter()
            .filter(|(_, v)| !v.settled)
            .map(|(c, v)| match v.through {
                Some(t) => format!("{c} 對到 {t}"),
                None => format!("{c} 猶未對過"),
            })
            .collect(),
        false => Vec::new(),
    };

    // Records before the bank's history begins cannot be checked.
    let since = schedule.and_then(|s| s.since);
    let unchecked: Vec<&Record> = records
        .iter()
        .filter(|r| since.is_none_or(|s| r.date >= s))
        .filter(|r| {
            let through = vouched.get(r.commodity.as_str()).and_then(|v| v.through);
            r.unverified || through.is_none_or(|t| r.date > t)
        })
        .collect();

    let (cadence, grace) = (held.cadence(), held.grace());
    let reference = held.reference(checked);
    let missed = cadence.missed(reference, today);
    let status = held.standing(today);
    let first = records
        .iter()
        .map(|r| r.date)
        .chain(figures.iter().map(|f| f.period_start.unwrap_or(f.as_of)))
        .min();
    let mark = |d: NaiveDate| Mark::new(d, today);
    let bank = schedule.and_then(|s| s.bank.clone()).unwrap_or_else(|| parent(crumbs));
    let group = label(crumbs.get(1).or(crumbs.last()));

    AccountRow {
        account: account.to_string(),
        label: name(crumbs, &group, &bank),
        bank,
        group,
        cadence,
        grace,
        status,
        wanted: missed.and_then(|m| wanted(cadence, m)),
        late_days: missed.filter(|_| status == Standing::Overdue).map(|m| m.late_days(today)),
        checked: checked.map(mark),
        per_commodity,
        recorded: records.iter().map(|r| r.date).max().map(mark),
        first: first.map(mark),
        tail: tail(&unchecked, held.journal.then_some(*account)),
        gaps: gaps
            .iter()
            .filter(|g| in_subtree(account, &g.account))
            .map(|g| GapSpan { from: mark(g.from), to: mark(g.to) })
            .collect(),
        since: since.map(mark),
        balances: held.balances(),
        instalment: (cadence == Cadence::Instalment).then(|| instalment(records)).flatten(),
        split: splits.covers(account).then(|| split(records, base, today)),
    }
}

fn label(crumb: Option<&Crumb>) -> String { crumb.map(|c| c.label.clone()).unwrap_or_default() }

/// The parent's label: `Assets:Bank:Alpha:Savings` files under 甲銀行, and a
/// card straight under `Liabilities:Card` under 信用卡 until the reader names
/// its bank. An account straight under a root files under its own label.
fn parent(crumbs: &[Crumb]) -> String {
    match crumbs {
        [_, .., parent, _] => parent.label.clone(),
        _ => label(crumbs.last()),
    }
}

/// What a bar is called under its group: the labels below the group, less
/// any that repeat the group or the bank shown beside it
/// (`Assets:Split:Alpha:Split` under 分帳 is 甲公司, not 分帳).
fn name(crumbs: &[Crumb], group: &str, bank: &str) -> String {
    let below = crumbs.get(2..).unwrap_or_default();
    let named: Vec<&str> =
        below.iter().map(|c| c.label.as_str()).filter(|l| *l != group && *l != bank).collect();
    match named.is_empty() {
        false => named.join(" "),
        true => label(below.first().or(crumbs.last())),
    }
}

impl Mark {
    fn new(date: NaiveDate, today: NaiveDate) -> Self {
        Mark { date: date.to_string(), ago: (today - date).num_days() }
    }
}

/// The statements the reader should fetch: `10 月的數單`, `8–10 月的數單`,
/// `第 3 季的數單`, `2025 年的數單`. A count or a payment is plain from the
/// bar.
fn wanted(cadence: Cadence, m: Missed) -> Option<String> {
    let wording: Option<(&str, &str, fn(NaiveDate) -> u32)> = match cadence {
        Cadence::Monthly { .. } => Some(("", " 月", |d| d.month())),
        Cadence::Quarterly => Some(("第 ", " 季", |d| d.month0() / 3 + 1)),
        Cadence::Yearly { .. } => Some(("", " 年", |d| d.year().unsigned_abs())),
        _ => None,
    };
    wording.map(|(prefix, unit, number)| {
        let due = number(m.due);
        match m.since.map(number).filter(|since| *since != due) {
            Some(since) => format!("{prefix}{since}–{due}{unit}的數單"),
            None => format!("{prefix}{due}{unit}的數單"),
        }
    })
}

fn tail(records: &[&Record], journal_account: Option<&str>) -> Option<Tail> {
    let first = records.iter().map(|r| r.date).min();
    first.map(|first| {
        let link = journal_account.map(|account| {
            let query = JournalQuery {
                from: Some(first.to_string()),
                unverified: records.iter().all(|r| r.unverified),
                ..JournalQuery::account(account)
            };
            format!("/journal{query}")
        });
        let count = records.iter().map(|r| r.id).collect::<BTreeSet<_>>().len();
        Tail { count, link }
    })
}

/// The first of each month from the earliest bar to today, for the axis.
fn months(rows: &[AccountRow], today: NaiveDate) -> Vec<Tick> {
    let start = today.checked_sub_days(Days::new(oldest(rows).max(0) as u64)).unwrap_or(today);
    std::iter::successors(start.with_day(1).map(|d| d + Months::new(1)), |d| {
        Some(*d + Months::new(1))
    })
    .take_while(|d| *d <= today)
    .map(|d| Tick { year: d.year(), month: d.month(), ago: (today - d).num_days() })
    .collect()
}

/// Payments so far against what is left: each payment is a record paying the
/// account down, and what is left at the last payment's size gives the rest.
fn instalment(records: &[Record]) -> Option<Instalment> {
    let payments: Vec<&Record> = records.iter().filter(|r| r.amount > Decimal::ZERO).collect();
    let owed: Decimal = records.iter().map(|r| r.amount).sum();
    let (first, last) = (payments.first()?, payments.last()?);
    let left = match owed < Decimal::ZERO {
        true => (-owed / last.amount).ceil().to_u32().unwrap_or(0),
        false => 0,
    };
    let paid = payments.len() as u32;
    Some(Instalment {
        paid,
        total: paid + left,
        first: first.date.to_string(),
        last: (last.date + Months::new(left)).to_string(),
    })
}

/// What a split account stands at: who owes whom, and since when it has
/// been open — the day after it last stood at zero.
fn split(records: &[Record], base: Currency, today: NaiveDate) -> SplitOpen {
    let mut running: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut settled_at = None;
    for (i, r) in records.iter().enumerate() {
        *running.entry(&r.commodity).or_default() += r.amount;
        if running.values().all(|v| v.is_zero()) {
            settled_at = Some(i);
        }
    }
    let open = &records[settled_at.map_or(0, |i| i + 1)..];
    let owed = running
        .iter()
        .filter(|(_, v)| !v.is_zero())
        .filter_map(|(c, v)| c.parse().ok().map(|c| Money::new(*v, c, base)))
        .collect();
    SplitOpen { owed, since: open.first().map(|r| Mark::new(r.date, today)) }
}
