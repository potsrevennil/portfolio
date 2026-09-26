//! 帳戶: every account a person files under, closed ones marked, as a tree
//! that folds; and closing or reopening one.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos_meta::Title;

use crate::{
    fold::{is_open, on_toggle, remembered},
    model::{AccountNode, AccountTree, JournalQuery},
    review::ActionError,
};

/// Where the open groups are remembered across visits, per browser.
const OPEN_KEY: &str = "accounts-open";

/// What 結清 does, for the button's tooltip.
const CLOSE_HINT: &str =
    "不再使用這個帳戶：選單裡不再出現，也不能再記帳到它。餘額要是 0 才能結清；之後可以重新啟用。";

#[server]
pub async fn load_accounts() -> Result<AccountTree, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let chart = crate::entries::Chart::load(&pool).await.map_err(crate::review::failed)?;
    let mut shown: Vec<_> = chart
        .accounts
        .iter()
        .filter_map(|a| chart.choice(a).map(|c| (a.account_type, c)))
        .collect();
    shown.sort_by(|(ta, a), (tb, b)| (ta, &a.path).cmp(&(tb, &b.path)));
    Ok(shown.into_iter().map(|(_, c)| c).collect())
}

#[server]
pub async fn close_account(path: String, note: Option<String>) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    db::account_status::close(&pool, &path, note.as_deref()).await.map_err(crate::review::failed)
}

#[server]
pub async fn reopen_account(path: String) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    db::account_status::reopen(&pool, &path, None).await.map_err(crate::review::failed)
}

#[derive(Clone, Copy)]
struct Controls {
    close: ServerAction<CloseAccount>,
    reopen: ServerAction<ReopenAccount>,
    open: RwSignal<BTreeSet<String>>,
}

#[component]
pub fn AccountsPage() -> impl IntoView {
    let close = ServerAction::<CloseAccount>::new();
    let reopen = ServerAction::<ReopenAccount>::new();
    let accounts =
        Resource::new(move || (close.version().get(), reopen.version().get()), |_| load_accounts());
    view! {
        <Title text="帳戶" />
        <div class="journal accounts">
            <ActionError result=close.value() />
            <ActionError result=reopen.value() />
            <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
                {move || Suspend::new(async move {
                    match accounts.await {
                        Ok(tree) => view! { <AccountsTree tree close reopen /> }.into_any(),
                        Err(e) => view! { <crate::error::LoadFailed error=e /> }.into_any(),
                    }
                })}
            </Transition>
        </div>
    }
}

#[component]
fn AccountsTree(
    tree: AccountTree,
    close: ServerAction<CloseAccount>,
    reopen: ServerAction<ReopenAccount>,
) -> impl IntoView {
    // The roots start open; the groups under them start folded.
    let roots: BTreeSet<String> = tree.0.iter().map(|n| n.account.path.clone()).collect();
    let mut groups = BTreeSet::new();
    collect_groups(&tree.0, &mut groups);
    let open = remembered(OPEN_KEY, roots);
    let controls = Controls { close, reopen, open };
    view! {
        <div class="tools">
            <span class="as-of"></span>
            <button type="button" on:click={
                let groups = groups.clone();
                move |_| open.set(groups.clone())
            }>"全部展開"</button>
            <button type="button" on:click=move |_| open.set(BTreeSet::new())>"全部收合"</button>
        </div>
        <div class="account-tree">{tree.0.into_iter().map(|n| node(n, controls)).collect_view()}</div>
    }
}

/// Paths of the accounts that fold (have accounts under them).
fn collect_groups(nodes: &[AccountNode], out: &mut BTreeSet<String>) {
    for n in nodes.iter().filter(|n| !n.children.is_empty()) {
        out.insert(n.account.path.clone());
        collect_groups(&n.children, out);
    }
}

fn node(n: AccountNode, c: Controls) -> AnyView {
    let a = n.account;
    let indent = format!("padding-left: calc({} * 1.1rem + 0.6rem)", a.depth);
    let level = format!("row account-row level-{}", a.depth.min(2));
    let href = format!("/journal{}", JournalQuery::account(&a.path));
    let path = a.path.clone();
    let target = a.path.clone();
    // A root is the chart itself; it is never closed.
    let control = (a.depth > 0).then(move || match a.closed {
        true => view! {
            <ActionForm action=c.reopen>
                <input type="hidden" name="path" value=target.clone() />
                <button type="submit">"重新啟用"</button>
            </ActionForm>
        }
        .into_any(),
        false => view! {
            <ActionForm action=c.close>
                <input type="hidden" name="path" value=target.clone() />
                <button type="submit" title=CLOSE_HINT>
                    "結清"
                </button>
            </ActionForm>
        }
        .into_any(),
    });
    let row = view! {
        <span class="marker" aria-hidden="true"></span>
        <a class="name" href=href>
            {a.label}
        </a>
        {a.closed.then(|| view! { <span class="badge closed">"已結清"</span> })}
        {control}
    };
    if n.children.is_empty() {
        return view! {
            <div class=level class:closed=a.closed style=indent>
                {row}
            </div>
        }
        .into_any();
    }
    let children = n.children.into_iter().map(|child| node(child, c)).collect_view();
    view! {
        <details class="group" open=is_open(path.clone(), c.open) on:toggle=on_toggle(path, c.open)>
            <summary class=level class:closed=a.closed style=indent>
                {row}
            </summary>
            {children}
        </details>
    }
    .into_any()
}
