//! Builds the journal page from `db::journal`: the filter parsed from the URL,
//! one page of transactions, and legs named by their standalone labels.

use anyhow::{Context, Error as E, Result};
use db::{
    chart::standalone_labels,
    journal::{self, Filter},
    query::{in_subtree, AccountType},
    SqlitePool,
};
use ledger_types::currency::Currency;

use crate::model::{AccountChoice, Entry, Journal, JournalQuery, Leg, Money, Review};

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
            reviewed: match q.review.as_deref().unwrap_or_default().parse().map_err(E::msg)? {
                Review::Any => None,
                Review::Reviewed => Some(true),
                Review::Unreviewed => Some(false),
            },
            unverified: q.unverified,
        })
    }
}

pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Journal> {
    let filter = Filter::try_from(query)?;
    let accounts = journal::accounts(pool).await?;
    let tree = accounts.iter().map(|a| (a.path.as_str(), a.label.as_str())).collect();
    let labels = standalone_labels(&tree);
    let label = |path: &str| labels.get(path).cloned().unwrap_or_else(|| path.to_string());

    let total = journal::count(pool, &filter).await?;
    let pages = u32::try_from(total.div_ceil(u64::from(PAGE_SIZE)).max(1))?;
    // Past the last page is the last page.
    let page = query.page.clamp(1, pages);
    let offset = u64::from(page - 1) * u64::from(PAGE_SIZE);
    let found = journal::entries(pool, &filter, PAGE_SIZE, offset).await?;

    let focus = |path: &str| filter.account.as_deref().is_some_and(|root| in_subtree(path, root));
    let entries = found
        .into_iter()
        .map(|e| Entry {
            id: e.id,
            date: e.date.to_string(),
            payee: e.payee,
            narration: e.narration,
            reviewed: e.reviewed,
            unverified: e.unverified,
            legs: e
                .legs
                .into_iter()
                .map(|l| Leg {
                    label: label(&l.account),
                    money: Money::new(l.amount, l.currency, base),
                    focus: focus(&l.account),
                    path: l.account,
                })
                .collect(),
        })
        .collect();

    // Equity holds the loader's system accounts; nobody files under them.
    let mut shown: Vec<_> =
        accounts.iter().filter(|a| a.account_type != AccountType::Equity).collect();
    // Sections in balance-sheet order, as Fava lists them; paths within.
    shown.sort_by_key(|a| (a.account_type, a.path.as_str()));
    let choices = shown
        .into_iter()
        .map(|a| AccountChoice {
            path: a.path.clone(),
            label: label(&a.path),
            depth: a.path.matches(':').count(),
        })
        .collect();

    Ok(Journal {
        query: query.with_page(page),
        account: query.account.as_deref().map(label),
        accounts: choices,
        entries,
        total,
        pages,
    })
}
