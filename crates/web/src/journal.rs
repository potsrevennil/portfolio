//! 日記帳: every transaction, newest first, with its legs. The filter and the
//! list are components of their own so T9's review queue can reuse them.

use leptos::{prelude::*, wasm_bindgen::JsCast, web_sys::HtmlDetailsElement};
use leptos_meta::Title;
use leptos_router::{components::Form, hooks::use_query_map};

use crate::{
    balance_sheet::MoneyText,
    error::LoadFailed,
    model::{AccountChoice, AccountNode, Entry, Journal, JournalQuery, Review},
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
                                    tree=journal.tree.clone()
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

/// A row of the picker: an account, the link that filters to it, and — for a
/// branch — what files under it. Built once, so the parts that re-render while
/// a reader types hold no query of their own.
#[derive(Clone)]
struct Row {
    href: String,
    name: String,
    /// Open on arrival: an ancestor of the filtered account.
    open: bool,
    children: Vec<Row>,
}

impl Row {
    fn of(node: AccountNode, pick: &impl Fn(Option<&str>) -> String) -> Row {
        Row {
            href: pick(Some(&node.path)),
            name: node.label,
            open: node.open,
            children: node.children.into_iter().map(|child| Row::of(child, pick)).collect(),
        }
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
    /// The same accounts to browse a level at a time.
    tree: Vec<AccountNode>,
) -> impl IntoView {
    let pick = {
        let query = query.clone();
        move |path: Option<&str>| format!("{action}{}", query.with_account(path))
    };
    let clear = pick(None);
    let rows: Vec<Row> = tree.into_iter().map(|node| Row::of(node, &pick)).collect();
    // Every account by the name it is searched under, with the link to it.
    let searchable = StoredValue::new(
        accounts
            .into_iter()
            .map(|account| (pick(Some(&account.path)), account.to_string()))
            .collect::<Vec<_>>(),
    );

    // Picking an account renders this filter again, and the panel it was
    // picked from closes with it. The attribute is bound rather than written
    // once, so the fresh `false` reaches a `<details>` the reader opened.
    let dropped = RwSignal::new(false);
    // What the box holds. It starts as the filtered account's name and follows
    // the reader's typing, so picking an account puts that account in the box:
    // the value attribute alone would not, since a browser stops honouring it
    // once the reader has typed.
    let chosen = chosen.unwrap_or_default();
    let text = RwSignal::new(chosen.clone());
    // Browse the tree until the reader types; then search what they typed.
    let typing = RwSignal::new(false);
    let searching = move || typing.get() && !text.read().trim().is_empty();
    let combo: NodeRef<leptos::html::Span> = NodeRef::new();
    let box_ref: NodeRef<leptos::html::Input> = NodeRef::new();
    // Client only: what a reader typed lives in the element, where no attribute
    // reaches it. Picking an account renders this filter again with that
    // account's name, which has to be written to the element to be seen.
    Effect::new(move |_| {
        if let Some(input) = box_ref.get() {
            let showing = text.get();
            if input.value() != showing {
                input.set_value(&showing);
            }
        }
    });
    // Focus leaving the control is the reader's way out of the panel, which a
    // `<details>` on its own does not give them.
    let left = move |ev: leptos::ev::FocusEvent| {
        let combo = combo.get_untracked();
        let inside = ev
            .related_target()
            .and_then(|target| target.dyn_into::<leptos::web_sys::Node>().ok())
            .zip(combo)
            .is_some_and(|(node, combo)| combo.contains(Some(&node)));
        if !inside {
            dropped.set(false);
        }
    };

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
                    // One control: type to search the accounts, or open the
                    // tree and walk down it.
                    <span class="combo" node_ref=combo on:focusout=left>
                        <input
                            type="search"
                            name="account"
                            placeholder="全部"
                            node_ref=box_ref
                            value=chosen.clone()
                            on:input=move |ev| {
                                text.set(event_target_value(&ev));
                                typing.set(true);
                                dropped.set(true);
                            }
                            on:focus=move |_| dropped.set(true)
                            on:keydown=move |ev| {
                                if ev.key() == "Escape" {
                                    dropped.set(false);
                                }
                            }
                        />
                        <details
                            class="picker"
                            open=move || dropped.get()
                            on:toggle=move |ev| {
                                let now = event_target::<HtmlDetailsElement>(&ev).open();
                                if dropped.get_untracked() != now {
                                    dropped.set(now);
                                }
                            }
                        >
                            <summary>
                                <span class="marker" aria-hidden="true"></span>
                                <span class="sr-only">"帳戶目錄"</span>
                            </summary>
                            <div class="panel">
                                <ul class="tree" class:hidden=searching>
                                    <li><a class="pick" href=clear.clone()>"全部"</a></li>
                                    {rows.into_iter().map(branch).collect_view()}
                                </ul>
                                <ul class="matches" class:hidden=move || !searching()>
                                    {move || {
                                        let typed = text.read().trim().to_lowercase();
                                        let found = searchable
                                            .read_value()
                                            .iter()
                                            .filter(|(_, name)| name.to_lowercase().contains(&typed))
                                            .map(|(href, name)| {
                                                view! {
                                                    <li>
                                                        <a class="pick" href=href.clone()>
                                                            {name.clone()}
                                                        </a>
                                                    </li>
                                                }
                                            })
                                            .collect_view();
                                        view! { {found} }
                                    }}
                                </ul>
                            </div>
                        </details>
                    </span>
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

/// One account in the picker's tree. A branch's own name only folds it open:
/// a link inside a `<summary>` is followed in some browsers and swallowed by
/// the fold in others, so the row that picks the branch itself sits inside it,
/// where 全部 means the whole of it.
fn branch(row: Row) -> AnyView {
    if row.children.is_empty() {
        return view! {
            <li>
                <a class="pick" href=row.href>
                    {row.name}
                </a>
            </li>
        }
        .into_any();
    }
    let children = row.children.into_iter().map(branch).collect_view();
    view! {
        <li>
            <details class="group" open=row.open>
                <summary>
                    <span class="marker" aria-hidden="true"></span>
                    <span class="name">{row.name}</span>
                </summary>
                <ul>
                    <li><a class="pick all" href=row.href>"全部"</a></li>
                    {children}
                </ul>
            </details>
        </li>
    }
    .into_any()
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
