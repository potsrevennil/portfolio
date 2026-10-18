//! 待確認: the journal held to unreviewed records, each confirmed as it is.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::{
    journal::{JournalFilter, JournalList},
    model::{JournalQuery, Queue},
};

#[server]
pub async fn load_queue(query: JournalQuery) -> Result<Queue, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    crate::queue::load(&pool, &query, Currency::TWD).await.map_err(failed)
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

/// The server's words, as the page shows them.
#[cfg(feature = "ssr")]
pub fn failed(e: anyhow::Error) -> ServerFnError { ServerFnError::new(format!("{e:#}")) }

/// The actions every review page shares, provided once by the app so the
/// queue's count follows them.
#[derive(Clone, Copy)]
pub struct Actions {
    pub confirm: ServerAction<ConfirmEntry>,
}

impl Actions {
    pub fn new() -> Self { Actions { confirm: ServerAction::new() } }

    /// Changes whenever one of them completes.
    pub fn version(&self) -> usize { self.confirm.version().get() }
}

impl Default for Actions {
    fn default() -> Self { Self::new() }
}

/// The unreviewed count beside the queue's link.
#[component]
pub fn ReviewCount() -> impl IntoView {
    let actions = expect_context::<Actions>();
    let count = Resource::new(move || actions.version(), |_| review_count());
    view! {
        <Transition>
            {move || Suspend::new(async move {
                count.await.ok().filter(|n| *n > 0).map(|n| view! { <span class="count-badge">{n}</span> })
            })}
        </Transition>
    }
}

#[component]
pub fn ReviewPage() -> impl IntoView {
    let actions = expect_context::<Actions>();
    let params = use_query_map();
    let queue = Resource::new(
        move || (JournalQuery::from(&params.get()), actions.version()),
        |(query, _)| load_queue(query),
    );
    view! {
        <Title text="待確認" />
        <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match queue.await {
                    Ok(Queue { journal }) => {
                        view! {
                            <div class="journal review">
                                <ActionError result=actions.confirm.value() />
                                <JournalFilter
                                    action="/review"
                                    query=journal.query.clone()
                                    chosen=journal.chosen.clone()
                                    accounts=journal.accounts.clone()
                                    tree=journal.tree.clone()
                                    review=false
                                />
                                <JournalList action="/review" journal review=true />
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

/// Confirm as it stands.
#[component]
pub fn ReviewControls(id: i64) -> impl IntoView {
    let confirm = expect_context::<Actions>().confirm;
    view! {
        <div class="actions">
            <ActionForm action=confirm>
                <input type="hidden" name="id" value=id />
                <button type="submit">"確認"</button>
            </ActionForm>
        </div>
    }
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
