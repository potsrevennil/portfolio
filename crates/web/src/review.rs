//! 待確認: the journal held to unreviewed records, each confirmed as it is or
//! opened to edit or split, and the statement lines waiting for a person to
//! say which record each one is.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::{use_params_map, use_query_map};

use crate::{
    balance_sheet::MoneyText,
    journal::{EntryHead, EntryLegs, JournalFilter, JournalList, OriginNote},
    model::{AccountChoice, Choice, Editing, JournalQuery, LegInput, Queue},
};

/// Blank rows the editor offers for splitting a leg.
pub const SPARE_ROWS: usize = 2;

#[server]
pub async fn load_queue(query: JournalQuery) -> Result<Queue, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    crate::queue::load(&pool, &query, Currency::TWD).await.map_err(failed)
}

#[server]
pub async fn review_count() -> Result<u64, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    crate::queue::count(&pool).await.map_err(failed)
}

#[server]
pub async fn confirm_entry(id: i64) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    db::review::confirm(&pool, id).await.map_err(failed)
}

#[server]
pub async fn load_entry(id: i64) -> Result<Editing, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    crate::queue::editing(&pool, id, Currency::TWD).await.map_err(failed)
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
    db::review::edit(&pool, id, &edit).await.map_err(failed)?;
    if confirm {
        leptos_axum::redirect("/review");
    }
    Ok(())
}

#[server]
pub async fn choose_pairing(choice: i64, record: Option<i64>) -> Result<(), ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let record = record.ok_or_else(|| ServerFnError::new("請選一筆紀錄"))?;
    db::pairing::choose(&pool, choice, record).await.map_err(failed)
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
    pub choose: ServerAction<ChoosePairing>,
}

impl Actions {
    pub fn new() -> Self {
        Actions {
            confirm: ServerAction::new(),
            save: ServerAction::new(),
            choose: ServerAction::new(),
        }
    }

    /// Changes whenever one of them completes.
    pub fn version(&self) -> usize {
        self.confirm.version().get() + self.save.version().get() + self.choose.version().get()
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
                    Ok(Queue { journal, choices }) => {
                        view! {
                            <div class="journal review">
                                <ActionError result=actions.confirm.value() />
                                <Pairings choices />
                                <JournalFilter
                                    action="/review"
                                    query=journal.query.clone()
                                    accounts=journal.accounts.clone()
                                    review=false
                                />
                                <JournalList action="/review" journal review=true />
                            </div>
                        }
                            .into_any()
                    }
                    Err(e) => view! { <p class="note error">{crate::error::load_failed(&e)}</p> }.into_any(),
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

/// Statement lines the importer would not pair alone: pick the record each
/// one is, and the next import verifies it.
#[component]
pub fn Pairings(choices: Vec<Choice>) -> impl IntoView {
    let choose = expect_context::<Actions>().choose;
    (!choices.is_empty()).then(|| {
        view! {
            <section class="pairings">
                <h2>"待配對"</h2>
                <p class="note">"對帳單的這幾筆，同金額的紀錄不只一筆。請選是哪一筆，下次匯入時就會對上。"</p>
                <ActionError result=choose.value() />
                <ol>
                    {choices
                        .into_iter()
                        .map(|c| {
                            let chosen = c.chosen;
                            view! {
                                <li class="choice">
                                    <div class="line">
                                        <span class="date">{c.date}</span>
                                        <span class="account">{c.account}</span>
                                        <MoneyText money=c.money />
                                        <span class="narration">{c.description}</span>
                                    </div>
                                    {chosen.map(|_| view! { <p class="chosen">"已選，下次匯入時對帳"</p> })}
                                    <ActionForm action=choose>
                                        <input type="hidden" name="choice" value=c.id />
                                        <ul class="candidates">
                                            {c
                                                .candidates
                                                .into_iter()
                                                .map(|e| {
                                                    let id = e.id;
                                                    view! {
                                                        <li>
                                                            <label>
                                                                <input
                                                                    type="radio"
                                                                    name="record"
                                                                    value=id
                                                                    checked=chosen == Some(id)
                                                                />
                                                                <EntryHead entry=e.clone() />
                                                                <EntryLegs legs=e.legs />
                                                            </label>
                                                        </li>
                                                    }
                                                })
                                                .collect_view()}
                                        </ul>
                                        <button type="submit">"選這筆"</button>
                                    </ActionForm>
                                </li>
                            }
                        })
                        .collect_view()}
                </ol>
            </section>
        }
    })
}

#[component]
pub fn EditPage() -> impl IntoView {
    let actions = expect_context::<Actions>();
    let params = use_params_map();
    let id = move || params.read().get("id").and_then(|id| id.parse::<i64>().ok());
    let editing = Resource::new(
        move || (id(), actions.save.version().get()),
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
                    Err(e) => view! { <p class="note error">{crate::error::load_failed(&e)}</p> }.into_any(),
                }
            })}
        </Transition>
    }
}

/// The legs as rows to edit, and blank ones to split into. Plain form fields,
/// so the page works before the script loads.
#[component]
pub fn Editor(editing: Editing) -> impl IntoView {
    let save = expect_context::<Actions>().save;
    let Editing { entry, accounts, history } = editing;
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
    rows.extend(
        (0..SPARE_ROWS)
            .map(|_| (LegInput { currency: currency.clone(), ..Default::default() }, None)),
    );
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
                            <th>"帳戶"</th>
                            <th>"金額"</th>
                            <th>"幣別"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {rows
                            .into_iter()
                            .enumerate()
                            .map(|(i, (leg, origin))| leg_row(i, leg, origin, &accounts))
                            .collect_view()}
                    </tbody>
                </table>
                <p class="note">
                    "要拆開一筆：把它的金額改小，在空白行選帳戶、填上其餘的金額。每個幣別加起來要是 0。"
                </p>
                <div class="submit">
                    <label>
                        <input type="checkbox" name="confirm" value="1" checked=!reviewed />
                        "同時確認"
                    </label>
                    <button type="submit">"儲存"</button>
                    <a href="/review">"返回"</a>
                </div>
            </ActionForm>
            <ActionError result=save.value() />
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
    accounts: &[AccountChoice],
) -> impl IntoView {
    let field = |name: &str| format!("legs[{i}][{name}]");
    view! {
        <tr>
            <td>
                <input type="hidden" name=field("posting") value=leg.posting />
                <AccountSelect name=field("account") accounts=accounts.to_vec() selected=leg.account />
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

/// A plain select of accounts, indented by depth; a closed one is marked.
#[component]
pub fn AccountSelect(
    name: String,
    accounts: Vec<AccountChoice>,
    #[prop(optional)] selected: String,
    /// The text of the empty choice.
    #[prop(default = "—")]
    blank: &'static str,
) -> impl IntoView {
    view! {
        <select name=name>
            <option value="">{blank}</option>
            {accounts
                .into_iter()
                .map(|a| {
                    let is_selected = a.path == selected;
                    // Full-width spaces: a select draws no tree.
                    let closed = if a.closed { "（已結清）" } else { "" };
                    let text = format!("{}{}{closed}", "\u{3000}".repeat(a.depth), a.label);
                    view! {
                        <option value=a.path selected=is_selected>
                            {text}
                        </option>
                    }
                })
                .collect_view()}
        </select>
    }
}
