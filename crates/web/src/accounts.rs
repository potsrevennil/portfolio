//! 帳戶: every account a person files under, closed ones on request, and
//! closing or reopening one.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::{components::Form, hooks::use_query_map};

use crate::{model::AccountChoice, review::ActionError};

#[server]
pub async fn load_accounts(closed: bool) -> Result<Vec<AccountChoice>, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let chart = crate::entries::Chart::load(&pool).await.map_err(crate::review::failed)?;
    let mut shown: Vec<_> = chart
        .accounts
        .iter()
        .filter(|a| closed || !a.closed)
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

#[component]
pub fn AccountsPage() -> impl IntoView {
    let close = ServerAction::<CloseAccount>::new();
    let reopen = ServerAction::<ReopenAccount>::new();
    let params = use_query_map();
    let show_closed = move || params.read().get("closed").is_some_and(|v| !v.is_empty());
    let accounts = Resource::new(
        move || (show_closed(), close.version().get(), reopen.version().get()),
        |(closed, ..)| load_accounts(closed),
    );
    view! {
        <Title text="帳戶" />
        <div class="journal accounts">
            <Form method="GET" action="/accounts">
                <div class="filter">
                    <label>
                        <input type="checkbox" name="closed" value="1" checked=show_closed />
                        "顯示已結清"
                    </label>
                    <button type="submit">"套用"</button>
                </div>
            </Form>
            <ActionError result=close.value() />
            <ActionError result=reopen.value() />
            <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
                {move || Suspend::new(async move {
                    match accounts.await {
                        Ok(accounts) => {
                            view! {
                                <ul class="account-list">
                                    {accounts.into_iter().map(|a| row(a, close, reopen)).collect_view()}
                                </ul>
                            }
                                .into_any()
                        }
                        Err(e) => view! { <p class="note error">{crate::error::load_failed(&e)}</p> }.into_any(),
                    }
                })}
            </Transition>
        </div>
    }
}

fn row(
    a: AccountChoice,
    close: ServerAction<CloseAccount>,
    reopen: ServerAction<ReopenAccount>,
) -> impl IntoView {
    let indent = format!("padding-left: {}rem", a.depth as f32 * 1.1);
    let href = format!("/journal{}", crate::model::JournalQuery::account(&a.path));
    let path = a.path.clone();
    // A root is the chart itself; it is never closed.
    let control = (a.depth > 0).then(move || match a.closed {
        true => view! {
            <ActionForm action=reopen>
                <input type="hidden" name="path" value=path.clone() />
                <button type="submit">"重新啟用"</button>
            </ActionForm>
        }
        .into_any(),
        false => view! {
            <ActionForm action=close>
                <input type="hidden" name="path" value=path.clone() />
                <button type="submit">"結清"</button>
            </ActionForm>
        }
        .into_any(),
    });
    view! {
        <li class="account-row" class:closed=a.closed style=indent>
            <a class="name" href=href>
                {a.label}
            </a>
            {a.closed.then(|| view! { <span class="badge closed">"已結清"</span> })}
            {control}
        </li>
    }
}
