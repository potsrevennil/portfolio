//! 帳戶: every account a person files under, as a tree that folds, each with
//! what it holds; accounts at zero are left out unless asked for.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos_meta::Title;

use crate::{
    balance_sheet::MoneyText,
    fold::{is_open, on_toggle, remembered},
    model::{AccountBranch, AccountTree, JournalQuery},
};

/// Where the open groups are remembered across visits, per browser.
const OPEN_KEY: &str = "accounts-open";

#[server]
pub async fn load_accounts() -> Result<AccountTree, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let plumbing = expect_context::<crate::entries::Plumbing>();
    let today = chrono::Local::now().date_naive();
    crate::account_tree::load(&pool, &plumbing, today, Currency::TWD)
        .await
        .map_err(crate::review::failed)
}

#[component]
pub fn AccountsPage() -> impl IntoView {
    let accounts = Resource::new(|| (), |_| load_accounts());
    view! {
        <Title text="帳戶" />
        <div class="journal accounts">
            <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
                {move || Suspend::new(async move {
                    match accounts.await {
                        Ok(tree) => view! { <AccountsTree tree /> }.into_any(),
                        Err(e) => view! { <crate::error::LoadFailed error=e /> }.into_any(),
                    }
                })}
            </Transition>
        </div>
    }
}

#[component]
fn AccountsTree(tree: AccountTree) -> impl IntoView {
    // The roots start open; the groups under them start folded.
    let roots: BTreeSet<String> = tree.0.iter().map(|n| n.path.clone()).collect();
    let mut groups = BTreeSet::new();
    collect_groups(&tree.0, &mut groups);
    let open = remembered(OPEN_KEY, roots);
    let hide_zero = RwSignal::new(true);
    view! {
        <div class="tools">
            <label class="as-of">
                <input
                    type="checkbox"
                    prop:checked=hide_zero
                    on:change=move |ev| hide_zero.set(event_target_checked(&ev))
                />
                "不顯示餘額 0 的帳戶"
            </label>
            <button type="button" on:click={
                let groups = groups.clone();
                move |_| open.set(groups.clone())
            }>"全部展開"</button>
            <button type="button" on:click=move |_| open.set(BTreeSet::new())>"全部收合"</button>
        </div>
        <div class="account-tree" class:hide-zero=hide_zero>
            {tree.0.into_iter().map(|n| node(n, open)).collect_view()}
        </div>
    }
}

/// Paths of the accounts that fold (have accounts under them).
fn collect_groups(nodes: &[AccountBranch], out: &mut BTreeSet<String>) {
    for n in nodes.iter().filter(|n| !n.children.is_empty()) {
        out.insert(n.path.clone());
        collect_groups(&n.children, out);
    }
}

fn node(n: AccountBranch, open: RwSignal<BTreeSet<String>>) -> AnyView {
    let indent = format!("padding-left: calc({} * 1.1rem + 0.6rem)", n.depth);
    let level = format!("row account-row level-{}", n.depth.min(2));
    let href = format!("/journal{}", JournalQuery::account(&n.path));
    let row = view! {
        <span class="marker" aria-hidden="true"></span>
        <a class="name" href=href>
            {n.label}
        </a>
        <span class="figures">
            {n.balance.into_iter().map(|money| view! { <MoneyText money /> }).collect_view()}
        </span>
    };
    if n.children.is_empty() {
        return view! {
            <div class=level class:zero=n.zero style=indent>
                {row}
            </div>
        }
        .into_any();
    }
    let path = n.path.clone();
    let children = n.children.into_iter().map(|child| node(child, open)).collect_view();
    view! {
        <details
            class="group"
            class:zero=n.zero
            open=is_open(path.clone(), open)
            on:toggle=on_toggle(path, open)
        >
            <summary class=level style=indent>
                {row}
            </summary>
            {children}
        </details>
    }
    .into_any()
}
