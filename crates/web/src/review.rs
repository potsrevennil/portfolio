//! 待確認: the journal held to unreviewed records, each with what the import
//! suggests, confirmed as it is or opened to edit or split; where the ledger
//! disagrees with an outside figure; and what the last imports took as
//! confirmed, still open to change. Rows can be ticked and changed together.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::{use_params_map, use_query_map};

use crate::{
    account_box::{AccountBox, Accounts},
    journal::{entry, EntryHead, JournalFilter, JournalList, OriginNote},
    model::{Bulk, ChartAccount, Editing, Entry, Gap, JournalQuery, LegInput, Queue},
};

/// Blank rows the editor opens with for splitting a leg; ＋ adds more.
pub const SPARE_ROWS: usize = 2;

#[server]
pub async fn load_queue(query: JournalQuery) -> Result<Queue, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let unlisted = expect_context::<crate::entries::Unlisted>();
    crate::queue::load(&pool, &unlisted, &query, Currency::TWD).await.map_err(failed)
}

#[server]
pub async fn review_count() -> Result<u64, ServerFnError> {
    // The nav renders on every page, including the one pass that gathers
    // the route list, where no database is in context yet.
    let pool = use_context::<db::SqlitePool>().ok_or_else(|| ServerFnError::new("沒有資料庫"))?;
    crate::queue::count(&pool).await.map_err(failed)
}

#[server]
pub async fn confirm_entry(id: i64) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    db::review::confirm(&pool, id).await.map_err(failed)
}

#[server]
pub async fn unconfirm_entry(id: i64) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    db::review::unconfirm(&pool, id).await.map_err(failed)
}

#[server]
pub async fn load_entry(id: i64) -> Result<Editing, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let unlisted = expect_context::<crate::entries::Unlisted>();
    crate::queue::editing(&pool, &unlisted, id, Currency::TWD).await.map_err(failed)
}

/// Rewrites a transaction's legs and note; `confirm` marks it reviewed too,
/// and then the queue is where the reader goes next.
#[server]
pub async fn save_entry(
    id: i64,
    narration: Option<String>,
    legs: Vec<LegInput>,
    confirm: Option<String>,
) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let confirm = confirm.is_some();
    let edit = crate::queue::edit(narration, &legs, confirm).map_err(failed)?;
    let statements = expect_context::<db::review::StatementAccounts>();
    db::review::edit(&pool, &statements, id, &edit).await.map_err(failed)?;
    if confirm {
        leptos_axum::redirect("/review");
    }
    Ok(())
}

/// Takes a transaction out of the ledger, once the reader ticked that they
/// mean it; then the journal is where they go next.
#[server]
pub async fn delete_entry(id: i64, sure: Option<String>) -> Result<(), ServerFnError> {
    if sure.is_none() {
        return Err(ServerFnError::new("要先勾「確定刪除這筆」"));
    }
    let pool = expect_context::<db::SqlitePool>();
    db::review::delete(&pool, id).await.map_err(failed)?;
    leptos_axum::redirect("/journal");
    Ok(())
}

/// Confirms each ticked row as it stands.
#[server]
pub async fn confirm_rows(ids: String) -> Result<Bulk, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    crate::queue::each_row(&pool, &ids, |id| db::review::confirm(&pool, id)).await.map_err(failed)
}

/// Moves each ticked row's category to `account`.
#[server]
pub async fn move_rows(
    ids: String,
    account: String,
    confirm: Option<String>,
) -> Result<Bulk, ServerFnError> {
    if account.trim().is_empty() {
        return Err(ServerFnError::new("請揀口座"));
    }
    let pool = expect_context::<db::SqlitePool>();
    let statements = expect_context::<db::review::StatementAccounts>();
    let confirm = confirm.is_some();
    crate::queue::each_row(&pool, &ids, |id| {
        db::review::move_category(&pool, &statements, id, &account, confirm)
    })
    .await
    .map_err(failed)
}

/// The server's words, as the page shows them.
#[cfg(feature = "ssr")]
pub fn failed(e: anyhow::Error) -> ServerFnError { ServerFnError::new(format!("{e:#}")) }

/// The actions every review page shares, provided once by the app so the
/// queue's count follows them.
#[derive(Clone, Copy)]
pub struct Actions {
    pub confirm: ServerAction<ConfirmEntry>,
    pub save: ServerAction<SaveEntry>,
    pub delete: ServerAction<DeleteEntry>,
    pub unconfirm: ServerAction<UnconfirmEntry>,
    pub confirm_rows: ServerAction<ConfirmRows>,
    pub move_rows: ServerAction<MoveRows>,
}

/// The rows ticked on 待確認. Only that page provides it, so only its rows
/// offer a tick.
#[derive(Clone, Copy)]
pub struct Selection {
    pub ticked: RwSignal<BTreeSet<i64>>,
    /// What the last change to them came to.
    pub outcome: RwSignal<Option<Result<Bulk, ServerFnError>>>,
}

impl Actions {
    pub fn new() -> Self {
        Actions {
            confirm: ServerAction::new(),
            save: ServerAction::new(),
            delete: ServerAction::new(),
            unconfirm: ServerAction::new(),
            confirm_rows: ServerAction::new(),
            move_rows: ServerAction::new(),
        }
    }

    /// Changes whenever one of them completes.
    pub fn version(&self) -> usize {
        self.confirm.version().get()
            + self.save.version().get()
            + self.delete.version().get()
            + self.unconfirm.version().get()
            + self.confirm_rows.version().get()
            + self.move_rows.version().get()
    }
}

impl Default for Actions {
    fn default() -> Self { Self::new() }
}

/// The unreviewed count beside the queue's link.
#[component]
pub fn ReviewCount() -> impl IntoView {
    let actions = expect_context::<Actions>();
    let count = Resource::new(move || actions.version(), |_| review_count());
    view! { <CountBadge count class="count-badge" /> }
}

/// A count beside a nav link, absent while there is nothing to count.
#[component]
pub fn CountBadge(
    count: Resource<Result<u64, ServerFnError>>,
    class: &'static str,
) -> impl IntoView {
    view! {
        <Transition>
            {move || Suspend::new(async move {
                count.await.ok().filter(|n| *n > 0).map(|n| view! { <span class=class>{n}</span> })
            })}
        </Transition>
    }
}

#[component]
pub fn ReviewPage() -> impl IntoView {
    let actions = expect_context::<Actions>();
    let params = use_query_map();
    let selection =
        Selection { ticked: RwSignal::new(BTreeSet::new()), outcome: RwSignal::new(None) };
    provide_context(selection);
    // Out here, as the list below is rebuilt with each change. What a change
    // left alone stays ticked, to open or try again.
    for result in [actions.confirm_rows.value(), actions.move_rows.value()] {
        Effect::new(move |_| {
            if let Some(result) = result.get() {
                if let Ok(bulk) = &result {
                    selection.ticked.set(bulk.left.iter().map(|(id, _)| *id).collect());
                }
                selection.outcome.set(Some(result));
            }
        });
    }
    let queue = Resource::new(
        move || (JournalQuery::from(&params.get()), actions.version()),
        |(query, _)| load_queue(query),
    );
    view! {
        <Title text="待確認" />
        <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match queue.await {
                    Ok(Queue { journal, gaps, confirmed, accounts, parents }) => {
                        let page: Vec<i64> = journal.entries.iter().map(|e| e.id).collect();
                        view! {
                            <div class="journal review">
                                <ActionError result=actions.confirm.value() />
                                <Gaps gaps />
                                <JournalFilter
                                    action="/review"
                                    query=journal.query.clone()
                                    chosen=journal.chosen.clone()
                                    accounts=journal.accounts.clone()
                                    tree=journal.tree.clone()
                                    review=false
                                />
                                <Ticked page accounts parents />
                                <JournalList action="/review" journal review=true />
                                <Confirmed entries=confirmed />
                            </div>
                        }
                            .into_any()
                    }
                    Err(e) => view! { <crate::error::LoadFailed error=e /> }.into_any(),
                }
            })}
        </Transition>
    }
}

/// Confirm as it stands, or open it to edit.
#[component]
pub fn ReviewControls(id: i64) -> impl IntoView {
    let confirm = expect_context::<Actions>().confirm;
    view! {
        <div class="actions">
            <ActionForm action=confirm>
                <input type="hidden" name="id" value=id />
                <button type="submit">"確認"</button>
            </ActionForm>
            <a href=format!("/review/{id}")>"修改"</a>
        </div>
    }
}

/// Ticks every row of the page, and once any is ticked, changes them together.
#[component]
pub fn Ticked(
    page: Vec<i64>,
    accounts: Vec<ChartAccount>,
    parents: Vec<ChartAccount>,
) -> impl IntoView {
    let Selection { ticked, outcome } = expect_context::<Selection>();
    let Actions { confirm_rows, move_rows, .. } = expect_context::<Actions>();
    let accounts = Accounts::new(accounts, parents);
    let ids = move || ticked.with(|t| t.iter().map(i64::to_string).collect::<Vec<_>>().join(","));
    let count = move || ticked.with(BTreeSet::len);
    view! {
        <label class="tick-all">
            <input
                type="checkbox"
                on:change=move |ev| {
                    let on = event_target_checked(&ev);
                    let page = page.clone();
                    ticked
                        .update(|t| {
                            for id in page {
                                if on {
                                    t.insert(id);
                                } else {
                                    t.remove(&id);
                                }
                            }
                        });
                }
            />
            "揀這頁全部"
        </label>
        <BulkOutcome outcome />
        <div class="bulk" class:hidden=move || count() == 0>
            <span class="count">{move || format!("揀 {} 筆", count())}</span>
            <ActionForm action=confirm_rows>
                <input type="hidden" name="ids" prop:value=ids />
                <button type="submit">"確認"</button>
            </ActionForm>
            <ActionForm action=move_rows>
                <input type="hidden" name="ids" prop:value=ids />
                <AccountBox name="account".to_string() accounts placeholder="改去佗一个口座" />
                <label>
                    <input type="checkbox" name="confirm" value="1" checked=true />
                    "同時確認"
                </label>
                <button type="submit">"改"</button>
            </ActionForm>
        </div>
    }
}

/// What the last change to several rows came to.
#[component]
pub fn BulkOutcome(outcome: RwSignal<Option<Result<Bulk, ServerFnError>>>) -> impl IntoView {
    move || {
        outcome.get().map(|r| match r {
            Ok(Bulk { done, left }) => view! {
                <div class="note">
                    <p>{format!("處理好 {done} 筆")}</p>
                    <ul>
                        {left
                            .into_iter()
                            .map(|(_, why)| view! { <li class="error">{why}</li> })
                            .collect_view()}
                    </ul>
                </div>
            }
            .into_any(),
            Err(e) => view! { <p class="note error">{crate::error::said(&e)}</p> }.into_any(),
        })
    }
}

/// Where the ledger disagrees with a statement, a count or a broker.
#[component]
pub fn Gaps(gaps: Vec<Gap>) -> impl IntoView {
    (!gaps.is_empty()).then(|| {
        let rows = gaps
            .into_iter()
            .map(|g| view! { <li>{format!("{} {}", g.label, g.text)}</li> })
            .collect_view();
        view! { <ul class="gaps">{rows}</ul> }
    })
}

/// What the last imports took as confirmed, each still open to change.
#[component]
pub fn Confirmed(entries: Vec<Entry>) -> impl IntoView {
    (!entries.is_empty()).then(|| {
        let count = format!("頂擺匯入已確認 {}", entries.len());
        let rows = entries.into_iter().map(|e| entry(e, false)).collect_view();
        view! {
            <details class="confirmed">
                <summary>{count}</summary>
                <ol class="entries">{rows}</ol>
            </details>
        }
    })
}

/// What the last run of an action refused, in the server's words.
#[component]
pub fn ActionError<T>(
    #[prop(into)] result: Signal<Option<Result<T, ServerFnError>>>,
) -> impl IntoView
where
    T: Clone + Send + Sync + 'static,
{
    move || {
        result.get().and_then(Result::err).map(|e| {
            view! { <p class="note error">{crate::error::said(&e)}</p> }
        })
    }
}

#[component]
pub fn EditPage() -> impl IntoView {
    let actions = expect_context::<Actions>();
    let params = use_params_map();
    let id = move || params.read().get("id").and_then(|id| id.parse::<i64>().ok());
    // A refused save leaves the page as it is, with what was filled in.
    let changes = Memo::new(move |seen: Option<&usize>| {
        let refused = actions.save.value().with(|saved| matches!(saved, Some(Err(_))));
        match (refused, seen) {
            (true, Some(seen)) => *seen,
            _ => actions.version(),
        }
    });
    let editing = Resource::new(
        move || (id(), changes.get()),
        |(id, _)| async move {
            match id {
                Some(id) => load_entry(id).await,
                None => Err(ServerFnError::new("查無交易")),
            }
        },
    );
    view! {
        <Title text="修改" />
        <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match editing.await {
                    Ok(editing) => view! { <Editor editing /> }.into_any(),
                    Err(e) => view! { <crate::error::LoadFailed error=e /> }.into_any(),
                }
            })}
        </Transition>
    }
}

/// The legs as rows to edit, and a blank one to split into, with ＋ for more.
/// Plain form fields, so one split works before the script loads.
#[component]
pub fn Editor(editing: Editing) -> impl IntoView {
    let Actions { save, delete, unconfirm, .. } = expect_context::<Actions>();
    let Editing { entry, accounts, parents, history } = editing;
    let accounts = Accounts::new(accounts, parents);
    let id = entry.id;
    let currency = entry.legs.first().map(|l| l.money.currency.to_string()).unwrap_or_default();
    let narration = entry.narration.clone().unwrap_or_default();
    let mut rows: Vec<(LegInput, Option<crate::model::Origin>)> = entry
        .legs
        .iter()
        .map(|l| {
            let input = LegInput {
                posting: l.id.to_string(),
                account: l.path.clone(),
                amount: l.exact.to_string(),
                currency: l.money.currency.to_string(),
            };
            (input, l.origin)
        })
        .collect();
    let blank = LegInput { currency, ..Default::default() };
    rows.extend((0..SPARE_ROWS).map(|_| (blank.clone(), None)));
    let opened = rows.len();
    let more = RwSignal::new(0);
    let reviewed = entry.reviewed;
    view! {
        <div class="journal edit">
            <div class="entry" class:unverified=entry.unverified>
                <EntryHead entry=entry.clone() source=true />
            </div>
            <ActionForm action=save>
                <input type="hidden" name="id" value=id />
                <label class="field">
                    "備註" <input type="text" name="narration" value=narration />
                </label>
                <table class="legs-edit">
                    <thead>
                        <tr>
                            <th>"口座"</th>
                            <th>"金額"</th>
                            <th>"幣別"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {rows
                            .into_iter()
                            .enumerate()
                            .map(|(i, (leg, origin))| leg_row(i, leg, origin, accounts))
                            .collect_view()}
                        <For
                            each=move || opened..opened + more.get()
                            key=|i| *i
                            children=move |i| leg_row(i, blank.clone(), None, accounts)
                        />
                    </tbody>
                </table>
                <button
                    type="button"
                    class="add-leg"
                    aria-label="加一个口座"
                    on:click=move |_| more.update(|n| *n += 1)
                >
                    "＋"
                </button>
                <p class="note">
                    "要拆開一筆：把它的金額改小，在空白行選口座、填上其餘的金額。每個幣別加起來要是 0。"
                </p>
                <div class="submit">
                    <label>
                        <input type="checkbox" name="confirm" value="1" checked=!reviewed />
                        "同時確認"
                    </label>
                    <button type="submit">"儲存"</button>
                    <a href=if reviewed { "/journal" } else { "/review" }>"返回"</a>
                </div>
            </ActionForm>
            <ActionError result=save.value() />
            {reviewed
                .then(|| {
                    view! {
                        <ActionForm action=unconfirm attr:class="unconfirm">
                            <input type="hidden" name="id" value=id />
                            <span class="note">"已確認。要重新檢查的話："</span>
                            <button type="submit">"改回未確認"</button>
                        </ActionForm>
                        <ActionError result=unconfirm.value() />
                    }
                })}
            <ActionForm action=delete attr:class="delete">
                <input type="hidden" name="id" value=id />
                <label>
                    <input type="checkbox" name="sure" value="1" />
                    "確定刪除這筆"
                </label>
                <button type="submit">"刪除"</button>
            </ActionForm>
            <ActionError result=delete.value() />
            {(!history.is_empty())
                .then(|| {
                    view! {
                        <section class="history">
                            <h3>"修改紀錄"</h3>
                            <ol>
                                {history
                                    .into_iter()
                                    .map(|c| {
                                        let what = match c.kind.as_str() {
                                            "entered" => "手動記帳",
                                            "edited" => "修改",
                                            "split" => "拆開",
                                            "confirmed" => "確認",
                                            "paired" => "選為配對",
                                            "deleted" => "刪除",
                                            "unconfirmed" => "改回未確認",
                                            other => other,
                                        }
                                        .to_string();
                                        view! {
                                            <li>
                                                <span class="date">{c.at}</span>
                                                " "
                                                {what}
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ol>
                        </section>
                    }
                })}
        </div>
    }
}

fn leg_row(
    i: usize,
    leg: LegInput,
    origin: Option<crate::model::Origin>,
    accounts: Accounts,
) -> impl IntoView {
    let field = |name: &str| format!("legs[{i}][{name}]");
    view! {
        <tr>
            <td>
                <input type="hidden" name=field("posting") value=leg.posting />
                <AccountBox name=field("account") accounts selected=leg.account />
                <OriginNote origin />
            </td>
            <td>
                <input type="text" inputmode="decimal" name=field("amount") value=leg.amount />
            </td>
            <td>
                <input type="text" class="currency" name=field("currency") value=leg.currency />
            </td>
        </tr>
    }
}
