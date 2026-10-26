//! 帳戶: every account as a tree with what it holds, over the queue ledger,
//! through the server function and the rendered page. All values are
//! invented.

mod common;

use common::{
    page::{position, visible},
    queue::{ledger, OLD_WALLET},
    server::{call, call_with, site},
};
use web::{
    accounts::load_accounts,
    entries::Plumbing,
    journal::load_journal,
    manual::load_manual,
    model::{AccountBranch, AccountTree, JournalQuery},
};

/// The branch at `path`, wherever it sits in the tree.
fn branch<'a>(tree: &'a AccountTree, path: &str) -> Option<&'a AccountBranch> {
    fn find<'a>(nodes: &'a [AccountBranch], path: &str) -> Option<&'a AccountBranch> {
        nodes.iter().find_map(|n| match n.path == path {
            true => Some(n),
            false => find(&n.children, path),
        })
    }
    find(&tree.0, path)
}

fn balance(tree: &AccountTree, path: &str) -> Vec<String> {
    let branch = branch(tree, path).unwrap_or_else(|| panic!("{path} not in {tree:?}"));
    branch.balance.iter().map(ToString::to_string).collect()
}

#[tokio::test]
async fn each_account_holds_what_it_and_those_under_it_hold() {
    let (_dir, pool) = ledger().await;
    let tree = call(&pool, load_accounts).await.unwrap();
    assert_eq!(balance(&tree, "Assets:Cash"), ["880 TWD"]);
    assert_eq!(balance(&tree, "Assets:Split"), ["200 TWD"]);
    // An emptied account holds nothing, and the filter hides it.
    let wallet = branch(&tree, OLD_WALLET).unwrap();
    assert!(wallet.balance.is_empty() && wallet.zero);
    assert!(!branch(&tree, "Assets").unwrap().zero);
    // Equity holds the loader's system accounts; it is not listed.
    assert!(branch(&tree, "Equity").is_none());
    // In a tree, the row above gives the context.
    assert_eq!(branch(&tree, "Assets:Split:Alpha:Tab").unwrap().label, "分帳");
}

#[tokio::test]
async fn plumbing_stays_out_of_the_tree_and_the_pickers() {
    let (_dir, pool) = ledger().await;
    let plumbing = || Plumbing::from_iter([OLD_WALLET]);
    let tree = call_with(&pool, plumbing(), load_accounts).await.unwrap();
    assert!(branch(&tree, OLD_WALLET).is_none());
    let journal =
        call_with(&pool, plumbing(), || load_journal(JournalQuery::default())).await.unwrap();
    assert!(!journal.accounts.iter().any(|a| a.path == OLD_WALLET));
    let manual = call_with(&pool, plumbing(), load_manual).await.unwrap();
    assert!(!manual.accounts.iter().any(|a| a.path == OLD_WALLET));
    // Its history still filters by it.
    let filtered =
        call_with(&pool, plumbing(), || load_journal(JournalQuery::account(OLD_WALLET))).await;
    assert_eq!(filtered.unwrap().total, 2);
}

#[tokio::test]
async fn the_accounts_page_renders() {
    let site = site().await;
    let accounts = site.page("/accounts").await;
    position(&visible(&accounts), "現金 880 TWD");
    position(&visible(&accounts), "不顯示餘額 0 的帳戶");
    // The emptied wallet is in the markup, hidden until the filter is off.
    assert!(accounts.contains(r#"<div class="account-tree hide-zero">"#), "{accounts}");
    let wallet = &accounts[..position(&accounts, "舊錢包")];
    let row = &wallet[wallet.rfind("<div").unwrap()..];
    assert!(row.contains("zero"), "{row}");
    // Every account, as a tree: the roots open, the groups under them folded.
    let assets = &accounts[position(&accounts, "資產")..];
    assert_eq!(accounts.matches(r#"<details open class="group">"#).count(), 4, "{accounts}");
    let folded = &assets[position(assets, r#"<details class="group">"#)..];
    position(&visible(folded), "分帳");
    position(&visible(&accounts), "全部展開");
}
