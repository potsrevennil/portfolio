//! Builds the journal page from `db::journal`: the filter parsed from the URL,
//! one page of transactions, and legs named by their standalone labels.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use db::{
    chart::standalone_labels,
    import::Origin as DbOrigin,
    journal::{self, Filter},
    query::{in_subtree, AccountType},
    SqlitePool,
};
use ledger::model::Source as DbSource;
use ledger_types::currency::Currency;

use crate::model::{
    AccountChoice, AccountKind, Entry, Journal, JournalQuery, Leg, Money, Origin, Review, Source,
};

pub const PAGE_SIZE: u32 = 50;

impl TryFrom<&JournalQuery> for Filter {
    type Error = anyhow::Error;

    fn try_from(q: &JournalQuery) -> Result<Self> {
        let date = |d: &Option<String>| {
            d.as_deref().map(|d| d.parse().with_context(|| format!("日期不對：{d}"))).transpose()
        };
        Ok(Filter {
            account: q.account.clone(),
            from: date(&q.from)?,
            to: date(&q.to)?,
            text: q.text.clone(),
            reviewed: match q
                .review
                .as_deref()
                .unwrap_or_default()
                .parse()
                .map_err(anyhow::Error::msg)?
            {
                Review::Any => None,
                Review::Reviewed => Some(true),
                Review::Unreviewed => Some(false),
            },
            unverified: q.unverified,
            source: q
                .source
                .as_deref()
                .map(|s| s.parse::<Source>().map_err(anyhow::Error::msg))
                .transpose()?
                .map(DbSource::from),
        })
    }
}

impl From<Source> for DbSource {
    fn from(s: Source) -> Self {
        match s {
            Source::Import => DbSource::Import,
            Source::Tiantian => DbSource::Tiantian,
            Source::Manual => DbSource::Manual,
        }
    }
}

impl From<DbSource> for Source {
    fn from(s: DbSource) -> Self {
        match s {
            DbSource::Import => Source::Import,
            DbSource::Tiantian => Source::Tiantian,
            DbSource::Manual => Source::Manual,
        }
    }
}

impl From<DbOrigin> for Origin {
    fn from(o: DbOrigin) -> Self {
        match o {
            DbOrigin::Tiantian => Origin::Tiantian,
            DbOrigin::Rule => Origin::Rule,
            DbOrigin::Fallback => Origin::Fallback,
            DbOrigin::Manual => Origin::Manual,
        }
    }
}

/// The chart, and each account's label as it reads outside the tree.
pub struct Chart {
    pub accounts: Vec<journal::Account>,
    labels: BTreeMap<String, String>,
}

impl Chart {
    pub async fn load(pool: &SqlitePool) -> Result<Self> {
        let accounts = journal::accounts(pool).await?;
        let tree = accounts.iter().map(|a| (a.path.as_str(), a.label.as_str())).collect();
        let labels = standalone_labels(&tree);
        Ok(Chart { accounts, labels })
    }

    pub fn label(&self, path: &str) -> String {
        self.labels.get(path).cloned().unwrap_or_else(|| path.to_string())
    }

    /// The accounts a person picks from, in balance-sheet order: no equity,
    /// which holds the loader's system accounts, and no closed account but
    /// those in `keep`, already in use where the picker is.
    pub fn choices(&self, keep: &[&str]) -> Vec<AccountChoice> {
        let mut shown: Vec<_> = self
            .accounts
            .iter()
            .filter(|a| a.account_type != AccountType::Equity)
            .filter(|a| !a.closed || keep.contains(&a.path.as_str()))
            .collect();
        shown.sort_by_key(|a| (a.account_type, a.path.as_str()));
        shown.into_iter().filter_map(|a| self.choice(a)).collect()
    }

    pub fn choice(&self, a: &journal::Account) -> Option<AccountChoice> {
        let kind = match a.account_type {
            AccountType::Asset => AccountKind::Asset,
            AccountType::Liability => AccountKind::Liability,
            AccountType::Income => AccountKind::Income,
            AccountType::Expense => AccountKind::Expense,
            AccountType::Equity => return None,
        };
        Some(AccountChoice {
            path: a.path.clone(),
            label: self.label(&a.path),
            depth: a.path.matches(':').count(),
            kind,
            closed: a.closed,
        })
    }

    /// One transaction as a page shows it; `focus` marks legs in the
    /// filtered subtree.
    pub fn entry(&self, e: journal::Entry, base: Currency, focus: Option<&str>) -> Entry {
        Entry {
            id: e.id,
            date: e.date.to_string(),
            payee: e.payee,
            narration: e.narration,
            source: e.source.into(),
            reviewed: e.reviewed,
            unverified: e.unverified,
            legs: e
                .legs
                .into_iter()
                .map(|l| Leg {
                    id: l.id,
                    label: self.label(&l.account),
                    money: Money::new(l.amount, l.currency, base),
                    exact: l.amount,
                    focus: focus.is_some_and(|root| in_subtree(&l.account, root)),
                    origin: l.origin.map(Origin::from),
                    path: l.account,
                })
                .collect(),
        }
    }
}

pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Journal> {
    let filter = Filter::try_from(query)?;
    let chart = Chart::load(pool).await?;

    let total = journal::count(pool, &filter).await?;
    let pages = u32::try_from(total.div_ceil(u64::from(PAGE_SIZE)).max(1))?;
    // Past the last page is the last page.
    let page = query.page.clamp(1, pages);
    let offset = u64::from(page - 1) * u64::from(PAGE_SIZE);
    let found = journal::entries(pool, &filter, PAGE_SIZE, offset).await?;
    let entries =
        found.into_iter().map(|e| chart.entry(e, base, filter.account.as_deref())).collect();

    // A closed account still filters its history once chosen.
    let keep: Vec<&str> = query.account.as_deref().into_iter().collect();
    Ok(Journal {
        query: query.with_page(page),
        account: query.account.as_deref().map(|a| chart.label(a)),
        accounts: chart.choices(&keep),
        entries,
        total,
        pages,
    })
}
