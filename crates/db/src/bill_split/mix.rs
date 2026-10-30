//! Never mix `Split:X` and `Split:X:A`: a group is netted or per-person, not
//! both.

use std::collections::BTreeSet;

use anyhow::Result;
use ledger::accounts::in_subtree;
use sqlx::SqliteConnection;

use super::{store::Owners, Books};

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
        let posted: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT a.path FROM postings p JOIN accounts a ON a.id = p.account_id",
        )
        .fetch_all(&mut *db)
        .await?;
        Ok(Uses {
            netted: owners.netted,
            owned: owners.owners.into_keys().collect(),
            posted: posted.into_iter().filter(|p| books.holds(p)).collect(),
        })
    }

    /// The account `path` already clashes with, if using it as `asked` would
    /// mix the two shapes.
    pub fn clash(&self, path: &str, asked: Asked) -> Option<&str> {
        let nested = |other: &&String| {
            other.as_str() != path && (in_subtree(other, path) || in_subtree(path, other))
        };
        let same_path = match asked {
            Asked::Netted => self.owned.get(path),
            Asked::Person => self.netted.get(path),
        };
        same_path
            .or_else(|| self.netted.iter().find(nested))
            .or_else(|| self.owned.iter().find(nested))
            .or_else(|| self.posted.iter().find(nested))
            .map(String::as_str)
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
