//! 帳戶: closing and reopening accounts over the queue ledger, through the
//! server functions and the rendered page. All values are invented.

mod common;

use common::{
    page::{position, visible},
    queue::{account_events, is_closed, ledger, FEE, INTO_WALLET, OLD_WALLET},
    server::{call, id_of, leg, legs, save, site},
};
use web::{
    accounts::{close_account, load_accounts, reopen_account},
    journal::load_journal,
    model::{AccountBranch, AccountTree, JournalQuery},
    review::delete_entry,
};

/// The branch at `path`, wherever it sits in the tree.
fn branch<'a>(tree: &'a AccountTree, path: &str) -> &'a AccountBranch {
    fn find<'a>(nodes: &'a [AccountBranch], path: &str) -> Option<&'a AccountBranch> {
        nodes.iter().find_map(|n| match n.account.path == path {
            true => Some(n),
            false => find(&n.children, path),
        })
    }
    find(&tree.0, path).unwrap_or_else(|| panic!("{path} not in {tree:?}"))
}

#[tokio::test]
async fn closing_an_account_marks_it_and_leaves_an_event() {
    let (_dir, pool) = ledger().await;
    // 結清 is offered only where it would succeed.
    let tree = call(&pool, load_accounts).await.unwrap();
    assert!(branch(&tree, OLD_WALLET).closable);
    assert!(!branch(&tree, "Assets:Cash").closable);
    assert!(!branch(&tree, "Assets:Split").closable);
    call(&pool, || close_account(OLD_WALLET.into(), Some("不用了".into()))).await.unwrap();
    assert!(is_closed(&pool, OLD_WALLET).await);
    let closed = ("closed".to_string(), Some("不用了".to_string()));
    assert_eq!(account_events(&pool, OLD_WALLET).await.last(), Some(&closed));

    // Gone from the pickers; still in the 帳戶 tree, marked closed.
    let journal = call(&pool, || load_journal(JournalQuery::default())).await.unwrap();
    assert!(!journal.accounts.iter().any(|a| a.path == OLD_WALLET));
    let tree = call(&pool, load_accounts).await.unwrap();
    let wallet = branch(&tree, OLD_WALLET);
    assert!(wallet.account.closed && !wallet.closable);
    // Its history still filters by it: the box names it, though the picker
    // no longer offers it.
    let filtered = call(&pool, || load_journal(JournalQuery::account(OLD_WALLET))).await.unwrap();
    assert_eq!(filtered.total, 2);
    assert_eq!(filtered.query.account.as_deref(), Some(OLD_WALLET));
    assert!(filtered.chosen.is_some());
    assert!(!filtered.accounts.iter().any(|a| a.path == OLD_WALLET));
    // Nothing new posts to it.
    let fee = id_of(&pool, FEE).await;
    let rows = legs(&pool, fee).await;
    let error = save(
        &pool,
        fee,
        FEE,
        vec![
            leg(Some(rows[0].0), "Assets:Bank:Savings", "-500"),
            leg(Some(rows[1].0), OLD_WALLET, "500"),
        ],
        false,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("帳戶已結清：Assets:Old-Wallet"), "{error}");

    call(&pool, || reopen_account(OLD_WALLET.into())).await.unwrap();
    let events: Vec<String> =
        account_events(&pool, OLD_WALLET).await.into_iter().map(|(e, _)| e).collect();
    assert_eq!(events, ["created", "closed", "reopened"]);
}

#[tokio::test]
async fn a_write_that_would_leave_money_in_a_closed_account_is_refused() {
    let (_dir, pool) = ledger().await;
    call(&pool, || close_account(OLD_WALLET.into(), None)).await.unwrap();
    // Deleting one of its two entries would leave the other's 70 in it.
    let into = id_of(&pool, INTO_WALLET).await;
    let held = call(&pool, || delete_entry(into, Some("1".into()))).await.unwrap_err();
    assert!(held.to_string().contains("會讓已結清的「舊錢包」剩 -70 TWD"), "{held}");
    assert_eq!(legs(&pool, into).await.len(), 2);
}

#[tokio::test]
async fn an_account_holding_money_is_not_closed() {
    let (_dir, pool) = ledger().await;
    let held = call(&pool, || close_account("Assets:Cash".into(), None)).await.unwrap_err();
    assert!(held.to_string().contains("餘額不是 0，不能結清：880 TWD"), "{held}");
    let parent = call(&pool, || close_account("Assets:Split".into(), None)).await.unwrap_err();
    assert!(parent.to_string().contains("底下還有 2 個帳戶沒結清"), "{parent}");
    let root = call(&pool, || close_account("Assets".into(), None)).await.unwrap_err();
    assert!(root.to_string().contains("這個帳戶不能結清"), "{root}");
}

/// What flowed through a category is not money it holds.
#[tokio::test]
async fn a_used_category_can_be_closed() {
    let (_dir, pool) = ledger().await;
    let tree = call(&pool, load_accounts).await.unwrap();
    assert!(branch(&tree, "Expenses:Food").closable);
    call(&pool, || close_account("Expenses:Food".into(), None)).await.unwrap();
    assert!(is_closed(&pool, "Expenses:Food").await);
}

#[tokio::test]
async fn the_accounts_page_renders() {
    let site = site().await;
    let accounts = site.page("/accounts").await;
    position(&visible(&accounts), "舊錢包 結清");
    assert!(!visible(&accounts).contains("現金 結清"), "{accounts}");
    // Equity holds the loader's system accounts; it is not listed.
    assert!(!accounts.contains("Equity"), "{accounts}");
    // Every account, as a tree: the roots open, the groups under them folded.
    let assets = &accounts[position(&accounts, "資產")..];
    assert_eq!(accounts.matches(r#"<details open class="group">"#).count(), 4, "{accounts}");
    let folded = &assets[position(assets, r#"<details class="group">"#)..];
    position(&visible(folded), "分帳");
    position(&visible(&accounts), "全部展開");
    // The button says what it does.
    assert!(accounts.contains("不再使用這個帳戶"), "{accounts}");
}
