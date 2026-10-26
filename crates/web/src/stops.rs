//! What stopped the last imports, which wrote nothing: each with what to fix
//! before the import is run again.

use leptos::prelude::*;

use crate::{
    balance_sheet::MoneyText,
    journal::{EntryHead, EntryLegs},
    model::{Entry, ImportStop, Unplaced},
};

#[component]
pub fn Stops(stops: Vec<ImportStop>) -> impl IntoView {
    (!stops.is_empty()).then(|| {
        view! {
            <section class="pairings stops">
                <ol>{stops.into_iter().map(|stop| view! { <Stop stop /> }).collect_view()}</ol>
            </section>
        }
    })
}

#[component]
fn Stop(stop: ImportStop) -> impl IntoView {
    let body = match stop {
        ImportStop::RecordAmbiguous { record, lines } => view! {
            <RecordLine record />
            <p class="why">"對帳單有幾若筆仝款，分袂出是佗一筆"</p>
            <Records entries=lines />
        }
        .into_any(),
        ImportStop::RecordUnexplained { record, through, lines } => view! {
            <RecordLine record />
            <p class="why">
                {format!("對帳單到 {through} 無這筆")}
            </p>
            {(!lines.is_empty()).then(|| view! { <p class="note">"可能是："</p> <Records entries=lines /> })}
        }
        .into_any(),
        ImportStop::NotOnStatement { entry, account, through, siblings } => {
            let missing = format!("「{account}」對帳單到 {through} 無這筆");
            let why = match siblings.is_empty() {
                true => missing,
                false => {
                    let elsewhere: Vec<String> = siblings.iter().map(|s| format!("「{s}」")).collect();
                    format!("{missing}，{}有仝款", elsewhere.join("、"))
                }
            };
            view! {
                <Record entry=entry.clone() />
                <p class="why">{why}</p>
                <Edit entry />
            }
            .into_any()
        }
        ImportStop::SharedDeferral { entry, account, other } => view! {
            <Record entry=entry.clone() />
            <p class="why">
                {format!(
                    "「{account}」對帳單猶未有，嘛記佇「{other}」：等後一期",
                )}
            </p>
            <Edit entry />
        }
        .into_any(),
        ImportStop::TwoDates { entry, dates } => {
            let days: Vec<String> = dates.iter().map(|(a, d)| format!("「{a}」{d}")).collect();
            view! {
                <Record entry />
                <p class="why">
                    {format!("兩間銀行紀錄無仝工：{}", days.join("、"))}
                </p>
            }
            .into_any()
        }
        ImportStop::SeveralOpenings { account } => view! {
            <p class="why">{format!("「{account}」有兩筆以上期初")}</p>
        }
        .into_any(),
        ImportStop::Unchained { account, day, ledger, statement } => view! {
            <p class="why">
                {format!(
                    "「{account}」{day} 餘額：帳裡 {ledger}，對帳單 {statement}，檢查下載檔",
                )}
            </p>
        }
        .into_any(),
    };
    view! { <li class="choice stop">{body}</li> }
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

#[component]
fn Records(entries: Vec<Entry>) -> impl IntoView {
    view! {
        <ul class="candidates">
            {entries.into_iter().map(|entry| view! { <li><Record entry /></li> }).collect_view()}
        </ul>
    }
}

#[component]
fn Edit(entry: Entry) -> impl IntoView {
    view! { <a href=format!("/review/{}", entry.id)>"修改"</a> }
}
