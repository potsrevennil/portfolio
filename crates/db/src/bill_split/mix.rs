//! Never mix `Split:X` and `Split:X:A`: a group is netted or per-person, not
//! both.

use std::{collections::BTreeSet, fmt};

use anyhow::Result;
use ledger::accounts::in_subtree;
use sqlx::SqliteConnection;

use super::{store::Owners, Books};

/// `account` was asked to share a group with `with`, in the other shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mixed {
    pub account: String,
    pub with: String,
}

impl fmt::Display for Mixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "「{}」和「{}」不可混用：一個群組要嘛算總額，要嘛算每個人",
            self.account, self.with
        )
    }
}

impl std::error::Error for Mixed {}

/// The first of `used` above or below `path`.
fn nested<'a>(used: impl IntoIterator<Item = &'a String>, path: &str) -> Option<&'a str> {
    used.into_iter()
        .find(|other| {
            other.as_str() != path && (in_subtree(other, path) || in_subtree(path, other))
        })
        .map(String::as_str)
}

/// Every account with postings that mixes with a netted group or a person's
/// account. The balance gate holds each write to this, so it binds however a
/// leg arrives. Accounts merely posted to don't mix with each other here: that
/// counts when someone asks to net or own one, as [`Uses::check`] does.
pub(crate) async fn mixed(db: &mut SqliteConnection) -> Result<Vec<Mixed>> {
    let owners = Owners::load(&mut *db).await?;
    let settled = || owners.netted.iter().chain(owners.owners.keys());
    Ok(posted(db)
        .await?
        .into_iter()
        .filter_map(|account| {
            let with = nested(settled(), &account)?.to_string();
            Some(Mixed { account, with })
        })
        .collect())
}

async fn posted(db: &mut SqliteConnection) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT a.path FROM postings p JOIN accounts a ON a.id = p.account_id",
    )
    .fetch_all(db)
    .await?)
}

/// What the split accounts are used for so far.
#[derive(Debug, Default)]
pub(super) struct Uses {
    pub netted: BTreeSet<String>,
    pub owned: BTreeSet<String>,
    /// Accounts with postings, whether or not any split knows them.
    pub posted: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Asked {
    Netted,
    Person,
}

impl Uses {
    pub async fn load(db: &mut SqliteConnection, books: &Books) -> Result<Self> {
        let owners = Owners::load(&mut *db).await?;
        let posted = posted(db).await?;
        Ok(Uses {
            netted: owners.netted,
            owned: owners.owners.into_keys().collect(),
            posted: posted.into_iter().filter(|p| books.holds(p)).collect(),
        })
    }

    /// Refuses using `path` as `asked` if that would mix the two shapes.
    pub fn check(&self, path: &str, asked: Asked) -> Result<(), Mixed> {
        match self.clash(path, asked) {
            Some(with) => Err(Mixed { account: path.to_string(), with: with.to_string() }),
            None => Ok(()),
        }
    }

    fn clash(&self, path: &str, asked: Asked) -> Option<&str> {
        let same_path = match asked {
            Asked::Netted => self.owned.get(path),
            Asked::Person => self.netted.get(path),
        };
        same_path
            .map(String::as_str)
            .or_else(|| nested(self.netted.iter().chain(&self.owned).chain(&self.posted), path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> BTreeSet<String> { paths.iter().map(|p| p.to_string()).collect() }

    #[test]
    fn a_group_cannot_be_both_netted_and_per_person() {
        let uses = Uses { netted: set(&["Split:X"]), ..Uses::default() };
        assert_eq!(uses.clash("Split:X:A", Asked::Person), Some("Split:X"));
        assert_eq!(uses.clash("Split:X", Asked::Person), Some("Split:X"));
        assert_eq!(uses.clash("Split:X", Asked::Netted), None);
        assert_eq!(uses.clash("Split:XY:A", Asked::Person), None);

        let uses = Uses { owned: set(&["Split:X:A"]), ..Uses::default() };
        assert_eq!(uses.clash("Split:X", Asked::Netted), Some("Split:X:A"));
        assert_eq!(uses.clash("Split:X:A", Asked::Netted), Some("Split:X:A"));
        assert_eq!(uses.clash("Split:X:B", Asked::Person), None);
    }

    #[test]
    fn postings_count_though_no_split_knows_them() {
        let uses = Uses { posted: set(&["Split:X:A"]), ..Uses::default() };
        assert_eq!(uses.clash("Split:X", Asked::Netted), Some("Split:X:A"));
        assert_eq!(uses.clash("Split:X:A", Asked::Person), None);

        let uses = Uses { posted: set(&["Split:X"]), ..Uses::default() };
        assert_eq!(uses.clash("Split:X:A", Asked::Person), Some("Split:X"));
        assert_eq!(uses.clash("Split:X", Asked::Netted), None);
    }
}
