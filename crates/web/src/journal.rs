//! 日記帳: every transaction, newest first, with its legs. The filter and the
//! list are components of their own so T9's review queue can reuse them.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::{components::Form, hooks::use_query_map};

use crate::{
    balance_sheet::MoneyText,
    error::LoadFailed,
    model::{AccountChoice, Entry, Journal, JournalQuery, Review},
};

#[server]
pub async fn load_journal(query: JournalQuery) -> Result<Journal, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    crate::entries::load(&pool, &query, Currency::TWD)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn JournalPage() -> impl IntoView {
    let params = use_query_map();
    let journal = Resource::new(move || JournalQuery::from(&params.get()), load_journal);
    view! {
        <Title text="日記帳" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match journal.await {
                    Ok(journal) => {
                        view! {
                            <div class="journal">
                                <JournalFilter
                                    action="/journal"
                                    query=journal.query.clone()
                                    chosen=journal.chosen.clone()
                                    accounts=journal.accounts.clone()
                                />
                                <JournalList action="/journal" journal />
                            </div>
                        }
                            .into_any()
                    }
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

/// A GET form, so a filtered list is a link that can be kept or shared.
#[component]
pub fn JournalFilter(
    action: &'static str,
    query: JournalQuery,
    /// The account box's contents: the name the picker offers the filtered
    /// account by, or nothing while every account is listed.
    chosen: Option<String>,
    accounts: Vec<AccountChoice>,
) -> impl IntoView {
    let current = query.review.clone().unwrap_or_default();
    let review = move |value: Review, text: &'static str| {
        let value = value.to_string();
        view! {
            <option value=value.clone() selected=current == value>
                {text}
            </option>
        }
    };
    view! {
        <Form method="GET" action=action>
            <div class="filter">
                <label>
                    "帳戶"
                    // A native list: the browser narrows it down as the reader
                    // types, with no script of ours to go wrong.
                    <input
                        type="search"
                        name="account"
                        list="accounts"
                        placeholder="全部"
                        value=chosen.unwrap_or_default()
                    />
                    <datalist id="accounts">
                        {accounts
                            .into_iter()
                            .map(|a| view! { <option value=a.to_string()></option> })
                            .collect_view()}
                    </datalist>
                </label>
                <label>
                    "從" <input type="date" name="from" value=query.from.clone().unwrap_or_default() />
                </label>
                <label>
                    "到" <input type="date" name="to" value=query.to.clone().unwrap_or_default() />
                </label>
                <label>
                    "搜尋" <input type="search" name="q" value=query.text.clone().unwrap_or_default() />
                </label>
                <label>
                    "確認"
                    <select name="review">
                        {review(Review::Any, "全部")}
                        {review(Review::Reviewed, "已確認")}
                        {review(Review::Unreviewed, "未確認")}
                    </select>
                </label>
                <label>
                    <input type="checkbox" name="unverified" value="1" checked=query.unverified />
                    "只看未對帳"
                </label>
                <button type="submit">"篩選"</button>
                <a href=action>"清除"</a>
            </div>
        </Form>
    }
}

/// One page of transactions and the links to the pages around it.
#[component]
pub fn JournalList(action: &'static str, journal: Journal) -> impl IntoView {
    let Journal { query, account, entries, total, pages, .. } = journal;
    let page = query.page;
    let link = move |page: u32| format!("{action}{}", query.with_page(page));
    let heading = account.map(|a| view! { <h2 class="account">{a}</h2> });
    let list = match entries.is_empty() {
        true => view! { <p class="note">"查無交易"</p> }.into_any(),
        false => view! { <ol class="entries">{entries.into_iter().map(entry).collect_view()}</ol> }
            .into_any(),
    };
    view! {
        {heading}
        <p class="count">{format!("共 {total} 筆 · 第 {page}／{pages} 頁")}</p>
        {list}
        <nav class="pager">
            {(page > 1).then(|| view! { <a href=link(page - 1) rel="prev">"較新"</a> })}
            {(page < pages).then(|| view! { <a href=link(page + 1) rel="next">"較舊"</a> })}
        </nav>
    }
}

fn entry(e: Entry) -> impl IntoView {
    view! {
        <li class="entry" class:unverified=e.unverified class:unreviewed=!e.reviewed>
            <div class="head">
                <span class="date">{e.date}</span>
                {e.payee.map(|p| view! { <span class="payee">{p}</span> })}
                {e.narration.map(|n| view! { <span class="narration">{n}</span> })}
                {e.unverified.then(|| view! { <span class="badge unverified">"未對帳"</span> })}
                {(!e.reviewed).then(|| view! { <span class="badge unreviewed">"未確認"</span> })}
            </div>
            <ul class="legs">
                {e
                    .legs
                    .into_iter()
                    .map(|l| {
                        let href = format!("/journal{}", JournalQuery::account(&l.path));
                        view! {
                            <li class="leg" class:focus=l.focus>
                                <a class="account" href=href>
                                    {l.label}
                                </a>
                                <MoneyText money=l.money />
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </li>
    }
}
