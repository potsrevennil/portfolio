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

use crate::model::{
    AccountChoice, AccountKind, AccountNode, Entry, Journal, JournalQuery, Leg, Money, Review,
};

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
    AccountChoice { path: path.to_string(), trail }
}

/// How many segments `path` has.
fn depth(path: &str) -> usize { path.matches(':').count() + 1 }

/// Every path from the root down to `path` itself.
fn prefixes(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices(':').map(|(i, _)| &path[..i]).chain(std::iter::once(path))
}

/// What a name the reader typed turned out to mean.
enum Named {
    Account(String),
    /// Several answer to it, by path.
    Several(BTreeSet<String>),
}

/// The chart as the journal page needs it: a name for every account, the names
/// the account box offers, and the way back from what the box holds to a path.
pub struct Chart {
    /// Path → the label that reads on its own, outside the tree.
    labels: BTreeMap<String, String>,
    /// Path → the label a tree shows, where the row above gives the context.
    tree: BTreeMap<String, String>,
    /// Path → the trail the picker offers it under.
    full: BTreeMap<String, String>,
    choices: Vec<AccountChoice>,
    /// The paths the picker offers, in the order it offers them.
    branches: Vec<String>,
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
                    _ => Named::Several(paths.iter().map(|p| p.to_string()).collect()),
                };
                (name, named)
            })
            .collect();

        let branches = offered.iter().map(|(a, _)| a.path.clone()).collect();
        let tree = tree.iter().map(|(p, l)| (p.to_string(), l.to_string())).collect();
        let choices = offered.into_iter().map(|(_, choice)| choice).collect();
        Ok(Chart { labels, tree, full, choices, names, branches })
    }

    pub fn label(&self, path: &str) -> String {
        self.labels.get(path).cloned().unwrap_or_else(|| path.to_string())
    }

    /// The name the account box holds for `path`: the trail the picker offers
    /// it under.
    pub fn offer(&self, path: &str) -> String {
        self.full.get(path).cloned().unwrap_or_else(|| path.to_string())
    }

    /// The offered accounts as a tree to browse one level at a time, opened
    /// down to `filtered`.
    pub fn nodes(&self, filtered: Option<&str>) -> Vec<AccountNode> { self.branch(None, filtered) }

    fn branch(&self, parent: Option<&str>, filtered: Option<&str>) -> Vec<AccountNode> {
        self.branches
            .iter()
            .filter(|path| match parent {
                Some(parent) => in_subtree(path, parent) && depth(path) == depth(parent) + 1,
                None => depth(path) == 1,
            })
            .map(|path| AccountNode {
                label: self.tree.get(path).cloned().unwrap_or_else(|| self.label(path)),
                open: filtered.is_some_and(|under| under != path && in_subtree(under, path)),
                children: self.branch(Some(path), filtered),
                path: path.clone(),
            })
            .collect()
    }

    /// What a query asks for. The filter is built here rather than from the
    /// query alone because the account box holds a name, and only the chart
    /// knows which account answers to it — so no caller can end up with a
    /// filter whose account quietly went missing.
    pub fn filter(&self, q: &JournalQuery) -> Result<Filter> {
        let date = |d: &Option<String>| {
            d.as_deref().map(|d| d.parse().with_context(|| format!("日期不對：{d}"))).transpose()
        };
        Ok(Filter {
            accounts: q
                .account
                .as_deref()
                .map(|a| self.resolve(a))
                .transpose()?
                .unwrap_or_default(),
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

    /// The accounts a typed name — or a path a link carried — means, usually
    /// one. A name nothing answers to is refused by name, never widened back
    /// to every account.
    pub fn resolve(&self, typed: &str) -> Result<Vec<String>> {
        match self.names.get(typed) {
            Some(Named::Account(path)) => Ok(vec![path.clone()]),
            Some(Named::Several(paths)) => Ok(paths.iter().map(|p| p.to_string()).collect()),
            None => Err(anyhow!("查無口座：{typed}")),
        }
    }

    /// `path` under the name the picker offers it by.
    fn choice(&self, path: &str) -> AccountChoice {
        AccountChoice {
            path: path.to_string(),
            trail: self.offer(path).split(" › ").map(str::to_string).collect(),
        }
    }
}

pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Journal> {
    let chart = Chart::load(pool).await?;
    let filter = chart.filter(query)?;

    let total = journal::count(pool, &filter).await?;
    let pages = u32::try_from(total.div_ceil(u64::from(PAGE_SIZE)).max(1))?;
    // Past the last page is the last page.
    let page = query.page.clamp(1, pages);
    let offset = u64::from(page - 1) * u64::from(PAGE_SIZE);
    let found = journal::entries(pool, &filter, PAGE_SIZE, offset).await?;

    let focus = |path: &str| filter.accounts.iter().any(|root| in_subtree(path, root));
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

    // One account is named by its path, so a link keeps working when the chart
    // is relabelled. Several are named by what the reader typed, which is the
    // only thing that means all of them.
    let one = match filter.accounts.as_slice() {
        [only] => Some(only.as_str()),
        _ => None,
    };
    let among = match one {
        Some(_) => Vec::new(),
        None => filter.accounts.iter().map(|p| chart.choice(p)).collect(),
    };
    Ok(Journal {
        query: JournalQuery {
            account: one.map(str::to_string).or_else(|| query.account.clone()),
            ..query.with_page(page)
        },
        account: one.map(|p| chart.label(p)).or_else(|| query.account.clone()),
        chosen: one.map(|p| chart.offer(p)).or_else(|| query.account.clone()),
        tree: chart.nodes(one),
        accounts: chart.choices,
        among,
        entries,
        total,
        pages,
    })
}
