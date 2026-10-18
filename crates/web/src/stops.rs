//! 匯入卡牢: what stopped the last imports, which wrote nothing, each with
//! the smallest step that settles it before the import is run again.

use leptos::prelude::*;

use crate::{
    balance_sheet::MoneyText,
    journal::{EntryHead, EntryLegs},
    model::{Entry, ImportStop, Place, StopKind, Unplaced},
    review::{ActionError, Actions},
};

/// Where a 天天記帳 record belongs: `place` is a radio value of
/// [`ImportStop`].
#[server]
pub async fn place_record(stop: i64, place: Option<String>) -> Result<(), ServerFnError> {
    use crate::review::failed;

    let pool = expect_context::<db::SqlitePool>();
    let place = place.ok_or_else(|| ServerFnError::new("請揀一个"))?;
    let pick = crate::queue::pick(&place).map_err(failed)?;
    db::stops::choose(&pool, stop, &pick).await.map_err(failed)
}

/// Moves a record its statement passes by to the sibling that shows it.
#[server]
pub async fn move_record(stop: i64, account: String) -> Result<(), ServerFnError> {
    use crate::review::failed;

    let pool = expect_context::<db::SqlitePool>();
    let statements = expect_context::<db::review::StatementAccounts>();
    db::stops::move_leg(&pool, &statements, stop, &account).await.map_err(failed)
}

/// Splits a transfer in two through the clearing account.
#[server]
pub async fn split_record(stop: i64) -> Result<(), ServerFnError> {
    use crate::review::failed;

    let pool = expect_context::<db::SqlitePool>();
    db::stops::detach(&pool, stop).await.map(|_| ()).map_err(failed)
}

#[component]
pub fn Stops(stops: Vec<ImportStop>) -> impl IntoView {
    let actions = expect_context::<Actions>();
    (!stops.is_empty()).then(|| {
        view! {
            <section class="pairings stops">
                <h2>"匯入卡牢"</h2>
                <p class="note">"頂擺匯入停落來，啥物攏無寫入。下底這幾筆處理好勢，才閣匯入一擺。"</p>
                <ActionError result=actions.place.value() />
                <ActionError result=actions.relocate.value() />
                <ActionError result=actions.split.value() />
                <ol>{stops.into_iter().map(|s| view! { <Stop stop=s /> }).collect_view()}</ol>
            </section>
        }
    })
}

#[component]
fn Stop(stop: ImportStop) -> impl IntoView {
    let ImportStop { id, kind, picked, resolved } = stop;
    let done = resolved.then(|| match &picked {
        Some(_) => view! { <p class="chosen">"已選，後一擺匯入就會對著"</p> },
        None => view! { <p class="chosen">"已處理，閣匯入一擺就好"</p> },
    });
    let body = match kind {
        StopKind::RecordAmbiguous { record, lines } => view! {
            <RecordLine record />
            <p class="why">"天天記帳這筆，對帳單有幾若筆仝款金額。是佗一筆？"</p>
            <Places id picked lines accounts=Vec::new() />
        }
        .into_any(),
        StopKind::RecordUnexplained { record, through, lines, accounts } => {
            let empty = lines.is_empty() && accounts.is_empty();
            view! {
                <RecordLine record />
                <p class="why">{format!("天天記帳這筆，對帳單到 {through} 攏對過矣，毋過無這筆。")}</p>
                {(!empty).then(|| view! { <Places id picked lines accounts /> })}
                <p class="note">"攏毋是的話，去天天記帳改好，才閣匯出。"</p>
            }
            .into_any()
        }
        StopKind::NotOnStatement { entry, account, through, siblings } => {
            let edit = format!("/review/{}", entry.id);
            view! {
                <Record entry />
                <p class="why">{format!("這筆猶未對帳，毋過「{account}」的對帳單到 {through} 攏無這筆。")}</p>
                <Moves id siblings />
                <p class="note">
                    "金額抑是日期無著的話：" <a href=edit>"修改"</a>
                </p>
            }
            .into_any()
        }
        StopKind::SharedDeferral { entry, account, other } => view! {
            <Record entry />
            <p class="why">
                {format!(
                    "這筆轉帳「{account}」的對帳單猶未出現，毋過伊嘛記佇「{other}」，規筆徙去後一期袂使得。拆做兩筆，逐筆家己對帳。",
                )}
            </p>
            <Split id />
        }
        .into_any(),
        StopKind::TwoDates { entry, dates } => {
            let days: Vec<String> = dates.iter().map(|(a, d)| format!("「{a}」{d}")).collect();
            view! {
                <Record entry />
                <p class="why">
                    {format!("這筆轉帳兩間銀行記佇無仝日：{}。拆做兩筆，逐筆用家己的日子。", days.join("、"))}
                </p>
                <Split id />
            }
            .into_any()
        }
        StopKind::SeveralOpenings { account } => view! {
            <p class="why">{format!("「{account}」有超過一筆期初，愛檢查匯入的檔案。")}</p>
        }
        .into_any(),
        StopKind::Unchained { account, day, ledger, statement } => view! {
            <p class="why">
                {format!(
                    "「{account}」{day} 的餘額，帳裡是 {ledger}，對帳單是 {statement}。下載的檔案可能對半中央開始、佮匯入過的重疊無齊，抑是欠一个檔案。",
                )}
            </p>
        }
        .into_any(),
    };
    view! { <li class="choice stop">{body} {done}</li> }
}

/// The 天天記帳 record, not in the ledger yet.
#[component]
fn RecordLine(record: Unplaced) -> impl IntoView {
    let what = [record.counter.join("、"), record.narration]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    view! {
        <div class="line">
            <span class="date">{record.date}</span>
            <span class="account">{record.account}</span>
            <MoneyText money=record.money />
            <span class="narration">{what}</span>
        </div>
    }
}

/// A record already in the ledger.
#[component]
fn Record(entry: Entry) -> impl IntoView {
    view! {
        <div class="entry" class:unverified=entry.unverified>
            <EntryHead entry=entry.clone() />
            <EntryLegs legs=entry.legs />
        </div>
    }
}

/// The statement lines the record may be, and the accounts it may book on.
#[component]
fn Places(
    id: i64,
    picked: Option<String>,
    lines: Vec<Entry>,
    accounts: Vec<Place>,
) -> impl IntoView {
    let place = expect_context::<Actions>().place;
    let is = move |value: &str| picked.as_deref() == Some(value);
    view! {
        <ActionForm action=place>
            <input type="hidden" name="stop" value=id />
            <ul class="candidates">
                {lines
                    .into_iter()
                    .map(|e| {
                        let value = ImportStop::line(e.id);
                        view! {
                            <li>
                                <label>
                                    <input type="radio" name="place" checked=is(&value) value=value />
                                    <EntryHead entry=e.clone() />
                                    <EntryLegs legs=e.legs />
                                </label>
                            </li>
                        }
                    })
                    .collect_view()}
                {accounts
                    .into_iter()
                    .map(|a| {
                        let value = ImportStop::account(&a.path);
                        view! {
                            <li>
                                <label>
                                    <input type="radio" name="place" checked=is(&value) value=value />
                                    <span>{format!("記佇「{}」，等伊的對帳單", a.label)}</span>
                                </label>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
            <button type="submit">"揀這个"</button>
        </ActionForm>
    }
}

/// One button per sibling account whose statement shows the record.
#[component]
fn Moves(id: i64, siblings: Vec<Place>) -> impl IntoView {
    let relocate = expect_context::<Actions>().relocate;
    view! {
        <div class="actions moves">
            {siblings
                .into_iter()
                .map(|s| {
                    view! {
                        <ActionForm action=relocate>
                            <input type="hidden" name="stop" value=id />
                            <input type="hidden" name="account" value=s.path />
                            <button type="submit">{format!("改記佇「{}」", s.label)}</button>
                        </ActionForm>
                    }
                })
                .collect_view()}
        </div>
    }
}

#[component]
fn Split(id: i64) -> impl IntoView {
    let split = expect_context::<Actions>().split;
    view! {
        <ActionForm action=split>
            <input type="hidden" name="stop" value=id />
            <button type="submit">"拆做兩筆"</button>
        </ActionForm>
    }
}
