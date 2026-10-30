//! One account's bar: its stretches, the dates written under them, and where
//! it stands.

use std::{collections::BTreeSet, sync::Arc};

use leptos::prelude::*;

use super::{
    forms::{Confirm, Editing, Panel, Settings},
    model::{AccountRow, CadenceName, Instalment, SplitOpen, Tail},
};
use crate::model::{account_href, Money};

/// Where `ago` falls on a bar spanning `span` days to today, in percent.
pub(super) fn at(ago: i64, span: f64) -> f64 {
    (100.0 * (1.0 - ago as f64 / span)).clamp(0.0, 100.0)
}

/// A date tag at `ago` on a bar: ending there when it labels what lies
/// before, starting there otherwise; kept inside the bar at either edge.
fn tag(ago: i64, before: bool, span: f64) -> String {
    let left = at(ago, span);
    let shift = match (before, left) {
        (_, l) if l < 12.0 => "0",
        (_, l) if l > 88.0 => "-100%",
        (true, _) => "-100%",
        (false, _) => "0",
    };
    format!("left:{left:.2}%;transform:translateX({shift})")
}

/// `left` and `width` covering `from`..`to` days ago.
fn stretch(from: i64, to: i64, span: f64) -> String {
    let (left, right) = (at(from, span), at(to, span));
    format!("left:{left:.2}%;width:{:.2}%", (right - left).max(0.0))
}

/// What goes on the bar, under it, and in its state line.
struct Parts {
    bar: AnyView,
    dates: AnyView,
    state: AnyView,
}

/// Paid down through card and bank statements, so no bar of its own to
/// check: progress from the first payment to the expected last.
fn instalment_parts(i: &Instalment) -> Parts {
    let width = 100.0 * f64::from(i.paid) / f64::from(i.total);
    let text = match i.paid >= i.total {
        true => format!("繳清矣，{} 期", i.total),
        false => format!("已繳 {}/{} 期", i.paid, i.total),
    };
    Parts {
        bar: view! { <i class="checked" style=format!("left:0%;width:{width:.2}%")></i> }
            .into_any(),
        dates: view! {
            <span class="checked" style="left:0">{i.first.clone()}</span>
            <span style="left:100%;transform:translateX(-100%)">{i.last.clone()}</span>
        }
        .into_any(),
        state: view! { <span class="status">{text}</span> }.into_any(),
    }
}

fn plain_parts(row: &AccountRow, span: Signal<f64>) -> Parts {
    let first = row.first.as_ref().map(|m| m.ago);
    let checked = row.checked.as_ref().map(|m| m.ago);
    let recorded = row.recorded.as_ref().map(|m| m.ago);
    let since = row.since.as_ref().map(|m| m.ago);
    let from = checked.or(since).or(first);
    let ran_past = recorded.is_some_and(|r| checked.is_none_or(|c| r < c));

    let green = first.zip(checked).map(|(f, c)| move || stretch(f, c, span.get()));
    // Before the bank's history: neither checked nor checkable.
    let before =
        first.zip(since).filter(|(f, s)| f > s).map(|(f, s)| move || stretch(f, s, span.get()));
    // A statement is due: from the last one to today, it has not come in.
    let missing =
        from.filter(|_| row.status.is_due()).map(|from| move || stretch(from, 0, span.get()));
    // Records past the last figure, or every checkable one when none checked it.
    let hatched =
        from.zip(recorded).filter(|_| ran_past).map(|(f, r)| move || stretch(f, r, span.get()));

    // The days written on the bar: where a split account opened; checked
    // through, under the green's end; recorded through, after it, where
    // records run past the check. One date once, where two fall on one day.
    let mut seen = BTreeSet::new();
    let tags = [
        row.split.as_ref().and_then(|s| s.since.clone()).map(|m| (m, "open", false)),
        row.checked.as_ref().map(|m| (m.clone(), "checked", true)),
        row.recorded.as_ref().filter(|_| ran_past).map(|m| (m.clone(), "recorded", false)),
    ]
    .into_iter()
    .flatten()
    .filter(|(mark, ..)| seen.insert(mark.date.clone()))
    .map(|(mark, class, leads)| {
        let text = mark.date.clone();
        view! {
            <span class=class style=move || tag(mark.ago, leads, span.get())>{text}</span>
        }
    })
    .collect_view();

    let status = match &row.split {
        Some(s) => split(s).into_any(),
        None => status_text(row).into_any(),
    };
    let per_commodity = (!row.per_commodity.is_empty())
        .then(|| view! { <small>{row.per_commodity.join(" · ")}</small> });
    let unchecked = row.tail.clone().map(tail);
    Parts {
        bar: view! {
            {before.map(|style| view! { <i class="before" style=style></i> })}
            {green.map(|style| view! { <i class="checked" style=style></i> })}
            {missing.map(|style| view! { <i class="missing" style=style></i> })}
            {hatched.map(|style| view! { <i class="recorded" style=style></i> })}
        }
        .into_any(),
        dates: tags.into_any(),
        state: view! { {status} {per_commodity} {unchecked} }.into_any(),
    }
}

#[component]
pub(super) fn AccountBar(
    row: AccountRow,
    span: Signal<f64>,
    editing: Editing,
    visible: Signal<bool>,
    banks: Arc<[String]>,
    today: String,
) -> AnyView {
    let row = Arc::new(row);
    let account = row.account.clone();
    let showing = {
        let account = account.clone();
        move |panel: Panel| {
            let account = account.clone();
            move || editing.with(|e| e.as_ref().is_some_and(|(a, p)| *a == account && *p == panel))
        }
    };
    let open = move |panel: Panel| {
        let account = account.clone();
        move |_| editing.set(Some((account.clone(), panel)))
    };
    let details = [
        Some(row.checked.as_ref().map_or("猶未對過".into(), |c| format!("對到 {}", c.date))),
        row.recorded.as_ref().map(|r| format!("記到 {}", r.date)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let Parts { bar, dates, state } = match &row.instalment {
        Some(i) => instalment_parts(i),
        None => plain_parts(&row, span),
    };
    let class = format!("account {}", row.status);
    // The bank, where the group does not already say it.
    let about = match row.bank == row.group || row.bank == row.label {
        true => CadenceName(row.cadence).to_string(),
        false => format!("{} · {}", row.bank, CadenceName(row.cadence)),
    };
    let (settings, confirm) = (row.clone(), row.clone());
    view! {
        <div class=class class:hidden=move || !visible.get()>
            <div class="name">
                <a href=account_href(&row.account)>{row.label.clone()}</a>
                <small>{about}</small>
            </div>
            <div class="bar" title=details>{bar}</div>
            <div class="dates">{dates}</div>
            <div class="state">
                {state}
                <span class="actions">
                    {row.confirmable().then(|| view! {
                        <button type="button" class="link" on:click=open(Panel::Confirm)>"手動對過"</button>
                    })}
                    <button type="button" class="link" on:click=open(Panel::Settings)>"設定"</button>
                </span>
            </div>
            <Show when=showing(Panel::Settings)>
                <Settings row=settings.clone() editing banks=banks.clone() />
            </Show>
            <Show when=showing(Panel::Confirm)>
                <Confirm row=confirm.clone() today=today.clone() editing />
            </Show>
        </div>
    }
    .into_any()
}

fn status_text(row: &AccountRow) -> impl IntoView {
    use ledger_types::cadence::Standing;

    let ask = row.wanted.as_ref().map(|what| format!("猶欠 {what}"));
    let (text, note) = match (row.status, row.late_days) {
        (Standing::Current, _) => (Some("對到最新".to_string()), None),
        (Standing::Due, _) => (ask, None),
        (Standing::Overdue, Some(days)) => (Some(format!("逾期 {days} 日")), ask),
        _ => (None, None),
    };
    view! {
        <span class="status">{text}</span>
        {note.map(|n| view! { <small>{n}</small> })}
    }
}

fn split(s: &SplitOpen) -> impl IntoView {
    let owed = s
        .owed
        .iter()
        .map(|m| {
            let whole = Money { amount: m.amount.abs(), currency: m.currency };
            match m.is_negative() {
                true => format!("你欠 {whole}"),
                false => format!("欠你 {whole}"),
            }
        })
        .collect::<Vec<_>>();
    let text = match owed.is_empty() {
        true => "清矣".to_string(),
        false => owed.join("、"),
    };
    view! { <span class="status">{text}</span> }
}

fn tail(t: Tail) -> AnyView {
    let text = format!("{} 筆猶未對", t.count);
    match t.link {
        Some(link) => view! { <a class="tail" href=link>{text}</a> }.into_any(),
        None => view! { <small>{text}</small> }.into_any(),
    }
}
