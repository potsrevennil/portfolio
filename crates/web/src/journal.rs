//! 日記帳: every transaction, newest first, with its legs. The filter and the
//! list are components of their own so T9's review queue can reuse them.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::{components::Form, hooks::use_query_map};

use crate::{
    balance_sheet::MoneyText,
    model::{AccountChoice, Entry, Journal, JournalQuery, Origin, Review, Source},
    review::ReviewControls,
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
                                    accounts=journal.accounts.clone()
                                />
                                <JournalList action="/journal" journal />
                            </div>
                        }
                            .into_any()
                    }
                    Err(e) => view! { <p class="note error">{crate::error::load_failed(&e)}</p> }.into_any(),
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
    accounts: Vec<AccountChoice>,
    /// Offer the review state; the queue lists only unreviewed records.
    #[prop(default = true)]
    review: bool,
) -> impl IntoView {
    let chosen = query.account.clone();
    let current = query.review.clone().unwrap_or_default();
    let option = move |current: String, value: String, text: &'static str| {
        view! {
            <option value=value.clone() selected=current == value>
                {text}
            </option>
        }
    };
    let review_option = {
        let current = current.clone();
        move |value: Review, text| option(current.clone(), value.to_string(), text)
    };
    let source = query.source.clone().unwrap_or_default();
    let source_option = move |value: Option<Source>, text| {
        option(source.clone(), value.map(|s| s.to_string()).unwrap_or_default(), text)
    };
    view! {
        <Form method="GET" action=action>
            <div class="filter">
                <label>
                    "帳戶"
                    <select name="account">
                        <option value="">"全部"</option>
                        {accounts
                            .into_iter()
                            .map(|a| {
                                let selected = chosen.as_deref() == Some(a.path.as_str());
                                // Full-width spaces: a select draws no tree.
                                let text = format!("{}{}", "\u{3000}".repeat(a.depth), a.label);
                                view! {
                                    <option value=a.path selected=selected>
                                        {text}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
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
                    "來源"
                    <select name="source">
                        {source_option(None, "全部")}
                        {source_option(Some(Source::Import), "對帳單")}
                        {source_option(Some(Source::Tiantian), "天天記帳")}
                        {source_option(Some(Source::Manual), "手動")}
                    </select>
                </label>
                {review
                    .then(|| {
                        view! {
                            <label>
                                "確認"
                                <select name="review">
                                    {review_option(Review::Any, "全部")}
                                    {review_option(Review::Reviewed, "已確認")}
                                    {review_option(Review::Unreviewed, "未確認")}
                                </select>
                            </label>
                        }
                    })}
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
pub fn JournalList(
    action: &'static str,
    journal: Journal,
    /// Each row carries its source and the queue's controls.
    #[prop(optional)]
    review: bool,
) -> impl IntoView {
    let Journal { query, account, entries, total, pages, .. } = journal;
    let page = query.page;
    let link = move |page: u32| format!("{action}{}", query.with_page(page));
    let heading = account.map(|a| view! { <h2 class="account">{a}</h2> });
    let list = match entries.is_empty() {
        true => view! { <p class="note">"查無交易"</p> }.into_any(),
        false => view! {
            <ol class="entries">
                {entries.into_iter().map(|e| entry(e, review)).collect_view()}
            </ol>
        }
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

fn entry(e: Entry, review: bool) -> impl IntoView {
    let id = e.id;
    view! {
        <li class="entry" class:unverified=e.unverified class:unreviewed=!e.reviewed>
            <EntryHead entry=e.clone() source=review />
            <EntryLegs legs=e.legs />
            {review.then(|| view! { <ReviewControls id /> })}
        </li>
    }
}

/// Date, payee, note and the states a record is in.
#[component]
pub fn EntryHead(
    entry: Entry,
    /// Show where it came from.
    #[prop(optional)]
    source: bool,
) -> impl IntoView {
    let e = entry;
    let source = source.then(|| {
        let text = match e.source {
            Source::Import => "對帳單",
            Source::Tiantian => "天天記帳",
            Source::Manual => "手動",
        };
        view! { <span class="badge source">{text}</span> }
    });
    view! {
        <div class="head">
            <span class="date">{e.date}</span>
            {e.payee.map(|p| view! { <span class="payee">{p}</span> })}
            {e.narration.map(|n| view! { <span class="narration">{n}</span> })}
            {source}
            {e.unverified.then(|| view! { <span class="badge unverified">"未對帳"</span> })}
            {(!e.reviewed).then(|| view! { <span class="badge unreviewed">"未確認"</span> })}
        </div>
    }
}

#[component]
pub fn EntryLegs(legs: Vec<crate::model::Leg>) -> impl IntoView {
    view! {
        <ul class="legs">
            {legs
                .into_iter()
                .map(|l| {
                    let href = format!("/journal{}", JournalQuery::account(&l.path));
                    view! {
                        <li class="leg" class:focus=l.focus>
                            <span class="account">
                                <a href=href>{l.label}</a>
                                <OriginNote origin=l.origin />
                            </span>
                            <MoneyText money=l.money />
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
}

/// Where a machine-chosen account came from; nothing for a person's choice.
#[component]
pub fn OriginNote(origin: Option<Origin>) -> impl IntoView {
    let text = match origin {
        Some(Origin::Tiantian) => Some("配對天天記帳"),
        Some(Origin::Rule) => Some("對應規則"),
        Some(Origin::Fallback) => Some("未分類"),
        Some(Origin::Manual) | None => None,
    };
    text.map(|t| view! { <span class="origin">{t}</span> })
}
