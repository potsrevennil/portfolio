//! Builds the journal page from `db::journal`: the filter parsed from the URL,
//! one page of transactions, and legs named by their standalone labels.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Context, Result};
use db::{
    chart::standalone_labels,
    journal::{self, Filter},
    query::{in_subtree, AccountType},
    SqlitePool,
};
use ledger_types::currency::Currency;

use crate::model::{AccountChoice, AccountKind, Entry, Journal, JournalQuery, Leg, Money, Review};

pub const PAGE_SIZE: u32 = 50;

/// 權益 holds the loader's system accounts; nobody files under them, so the
/// picker has no name for that kind and never offers it.
impl TryFrom<AccountType> for AccountKind {
    type Error = AccountType;

    fn try_from(account_type: AccountType) -> Result<Self, AccountType> {
        match account_type {
            AccountType::Asset => Ok(AccountKind::Asset),
            AccountType::Liability => Ok(AccountKind::Liability),
            AccountType::Income => Ok(AccountKind::Income),
            AccountType::Expense => Ok(AccountKind::Expense),
            AccountType::Equity => Err(account_type),
        }
    }
}

/// An account's ancestors' labels from the root down, then its own. Inside a
/// trail each parent gives the context, so the plain tree label is enough —
/// the standalone label would repeat the parent it already follows. A chart
/// that leaves a root unlabelled would read `Assets`; its kind names it
/// instead, since no page may fall back to English.
fn trail(path: &str, kind: Option<AccountKind>, tree: &BTreeMap<&str, &str>) -> AccountChoice {
    let leaf = |prefix: &str| prefix.rsplit(':').next().unwrap_or(prefix).to_string();
    let mut trail: Vec<String> = prefixes(path)
        .map(|prefix| tree.get(prefix).map(|l| l.to_string()).unwrap_or_else(|| leaf(prefix)))
        .collect();
    if let (Some(root), Some(kind)) = (trail.first_mut(), kind) {
        if *root == leaf(path.split(':').next().unwrap_or(path)) {
            *root = kind.to_string();
        }
    }
    AccountChoice { trail }
}

/// Every path from the root down to `path` itself.
fn prefixes(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices(':').map(|(i, _)| &path[..i]).chain(std::iter::once(path))
}

/// What a name the reader typed turned out to mean.
enum Named {
    Account(String),
    /// Several answer to it: the full names to pick from instead.
    Several(BTreeSet<String>),
}

/// The chart as the journal page needs it: a name for every account, the names
/// the account box offers, and the way back from what the box holds to a path.
pub struct Chart {
    /// Path → the label that reads on its own, outside the tree.
    labels: BTreeMap<String, String>,
    /// Path → the trail the picker offers it under.
    full: BTreeMap<String, String>,
    choices: Vec<AccountChoice>,
    /// Every text the box may hold, and what it names.
    names: BTreeMap<String, Named>,
}

impl Chart {
    pub async fn load(pool: &SqlitePool) -> Result<Self> {
        let accounts = journal::accounts(pool).await?;
        let tree: BTreeMap<&str, &str> =
            accounts.iter().map(|a| (a.path.as_str(), a.label.as_str())).collect();
        let labels = standalone_labels(&tree);
        let label = |path: &str| labels.get(path).cloned().unwrap_or_else(|| path.to_string());

        let mut offered: Vec<_> = accounts
            .iter()
            .filter_map(|a| match AccountKind::try_from(a.account_type) {
                Ok(kind) => Some((a, trail(&a.path, Some(kind), &tree))),
                Err(_) => None,
            })
            .collect();
        // Sections in balance-sheet order, as Fava lists them; paths within.
        offered.sort_by_key(|(a, _)| (a.account_type, a.path.as_str()));

        // Every account has a full name, not only the offered ones: a leg
        // links to the loader's own accounts too, so they still have to
        // resolve.
        let full: BTreeMap<String, String> = accounts
            .iter()
            .map(|a| {
                let kind = AccountKind::try_from(a.account_type).ok();
                (a.path.clone(), trail(&a.path, kind, &tree).to_string())
            })
            .collect();

        let mut found: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for account in &accounts {
            let path = account.path.as_str();
            for name in [path.to_string(), label(path), full[path].clone()] {
                found.entry(name).or_default().insert(path);
            }
        }
        let names = found
            .into_iter()
            .map(|(name, paths)| {
                let named = match paths.len() {
                    1 => Named::Account(paths.first().expect("one path").to_string()),
                    _ => Named::Several(paths.iter().map(|p| full[*p].clone()).collect()),
                };
                (name, named)
            })
            .collect();

        let choices = offered.into_iter().map(|(_, choice)| choice).collect();
        Ok(Chart { labels, full, choices, names })
    }

    pub fn label(&self, path: &str) -> String {
        self.labels.get(path).cloned().unwrap_or_else(|| path.to_string())
    }

    /// The name the picker offers `path` under: what the account box holds so
    /// that submitting the form again resolves back to this same account.
    pub fn full_name(&self, path: &str) -> String {
        self.full.get(path).cloned().unwrap_or_else(|| path.to_string())
    }

    /// The account a typed name — or a path a link carried — means. A name
    /// that resolves to no one account is refused by name, never widened back
    /// to every account.
    pub fn resolve(&self, typed: &str) -> Result<String> {
        match self.names.get(typed) {
            Some(Named::Account(path)) => Ok(path.clone()),
            Some(Named::Several(names)) => {
                let names: Vec<_> = names.iter().map(String::as_str).collect();
                Err(anyhow!("帳戶名重複：{}", names.join("、")))
            }
            None => Err(anyhow!("查無帳戶：{typed}")),
        }
    }
}

/// Everything the URL settles on its own. The account is left out: the box
/// holds a name, and only the chart knows which account answers to it — see
/// [`Chart::resolve`].
impl TryFrom<&JournalQuery> for Filter {
    type Error = anyhow::Error;

    fn try_from(q: &JournalQuery) -> Result<Self> {
        let date = |d: &Option<String>| {
            d.as_deref().map(|d| d.parse().with_context(|| format!("日期不對：{d}"))).transpose()
        };
        Ok(Filter {
            account: None,
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
        })
    }
}

pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Journal> {
    let chart = Chart::load(pool).await?;
    let filter = Filter {
        account: query.account.as_deref().map(|a| chart.resolve(a)).transpose()?,
        ..Filter::try_from(query)?
    };

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
                    label: chart.label(&l.account),
                    money: Money::new(l.amount, l.currency, base),
                    focus: focus(&l.account),
                    path: l.account,
                })
                .collect(),
        })
        .collect();

    Ok(Journal {
        // The path, not whichever name found it: a link keeps working when the
        // chart is relabelled.
        query: JournalQuery { account: filter.account.clone(), ..query.with_page(page) },
        account: filter.account.as_deref().map(|p| chart.label(p)),
        chosen: filter.account.as_deref().map(|p| chart.full_name(p)),
        accounts: chart.choices,
        entries,
        total,
        pages,
    })
}
