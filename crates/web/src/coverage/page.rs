//! The page: its filters, the axis, and the bars grouped by kind of account.

use std::{collections::BTreeSet, sync::Arc};

use leptos::prelude::*;
use leptos_meta::Title;
use serde::{Deserialize, Serialize};

use super::{
    actions::{coverage_due, load_coverage, CoverageActions},
    bar::{at, AccountBar},
    forms::Editing,
    model::{oldest, AccountRow, Coverage, Tick},
};
use crate::{error::LoadFailed, review::CountBadge};

/// Where the chosen span is remembered, per browser.
const RANGE_KEY: &str = "coverage-range";

/// The count of accounts awaiting a statement, beside 對帳進度 in the nav.
#[component]
pub fn CoverageCount() -> impl IntoView {
    let actions = expect_context::<CoverageActions>();
    let count = Resource::new(move || actions.version(), |_| coverage_due());
    view! { <CountBadge count class="count-badge due" /> }
}

#[component]
pub fn CoveragePage() -> impl IntoView {
    let actions = expect_context::<CoverageActions>();
    let coverage = Resource::new(move || actions.version(), |_| load_coverage());
    view! {
        <Title text="對帳進度" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match coverage.await {
                    Ok(coverage) => view! { <CoverageView coverage /> }.into_any(),
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

/// How far back the bars reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Range {
    Quarter,
    Year,
    All,
}

impl Range {
    const CHOICES: [(Range, &'static str); 3] =
        [(Range::Quarter, "3 個月"), (Range::Year, "1 年"), (Range::All, "全部")];

    /// In days; everything when all.
    fn days(self, oldest: i64) -> i64 {
        match self {
            Range::Quarter => 92,
            Range::Year => 366,
            Range::All => oldest.max(31),
        }
    }
}

fn chip(
    text: String,
    selected: impl Fn() -> bool + Send + Sync + 'static,
    pick: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <button type="button" class="chip" class:selected=selected on:click=move |_| pick()>
            {text}
        </button>
    }
}

#[component]
pub fn CoverageView(coverage: Coverage) -> impl IntoView {
    let hide_zero = RwSignal::new(true);
    let bank = RwSignal::new(None::<String>);
    let range = RwSignal::new(Range::Quarter);
    crate::stored::remember(RANGE_KEY, range);
    let editing = Editing::new(None);
    let Coverage { today, months, accounts } = coverage;
    let with_since = accounts.iter().any(|a| a.since.is_some());

    let oldest = oldest(&accounts);
    let span = Signal::derive(move || range.get().days(oldest) as f64);
    let every_bank: BTreeSet<String> = accounts.iter().map(|a| a.bank.clone()).collect();
    // Only banks with an account the zero filter leaves.
    let holding: BTreeSet<String> =
        accounts.iter().filter(|a| !a.zero()).map(|a| a.bank.clone()).collect();
    let banks: Arc<[String]> = every_bank.into_iter().collect();
    let shown = move |zero: bool, of: &str| {
        !(hide_zero.get() && zero) && bank.with(|b| b.as_deref().is_none_or(|b| b == of))
    };

    // In the chart's order, which the rows already follow.
    let mut groups: Vec<(String, Vec<AccountRow>)> = Vec::new();
    for a in accounts {
        match groups.last_mut() {
            Some((group, rows)) if *group == a.group => rows.push(a),
            _ => groups.push((a.group.clone(), vec![a])),
        }
    }
    let bank_chip = move |value: Option<String>| {
        let on = value.clone();
        let text = value.clone().unwrap_or_else(|| "全部".into());
        chip(text, move || bank.with(|b| *b == on), move || bank.set(value.clone()))
    };
    let chips = banks
        .iter()
        .cloned()
        .map(|b| {
            let held = holding.contains(&b);
            let when = move || !hide_zero.get() || held;
            view! { <Show when=when>{bank_chip(Some(b.clone()))}</Show> }
        })
        .collect_view();
    view! {
        <div class="coverage">
            <p class="as-of">{today.clone()}</p>
            <div class="tools">
                {bank_chip(None)}
                {chips}
                <label>
                    <input
                        type="checkbox"
                        checked=hide_zero
                        on:change=move |ev| hide_zero.set(event_target_checked(&ev))
                    />
                    "不顯示餘額 0 的帳戶"
                </label>
                <span class="spacer"></span>
                {Range::CHOICES
                    .into_iter()
                    .map(|(value, text)| {
                        chip(text.into(), move || range.get() == value, move || range.set(value))
                    })
                    .collect_view()}
            </div>
            <div class="legend">
                <span><i class="swatch checked"></i>"對過數單"</span>
                <span><i class="swatch gap"></i>"中央無數單"</span>
                <span><i class="swatch recorded"></i>"有記錄，猶未對"</span>
                <span><i class="swatch none"></i>"無記錄"</span>
                <span><i class="swatch missing"></i>"數單猶未匯入"</span>
                {with_since.then(|| view! {
                    <span><i class="swatch before"></i>"銀行無留的舊記錄"</span>
                })}
            </div>
            <Axis months span />
            {groups
                .into_iter()
                .map(|(name, rows)| {
                    let members: Vec<(bool, String)> =
                        rows.iter().map(|r| (r.zero(), r.bank.clone())).collect();
                    let any = move || members.iter().any(|(zero, of)| shown(*zero, of));
                    view! {
                        <section class="kind" class:hidden=move || !any()>
                            <h3>{name}</h3>
                            {rows
                                .into_iter()
                                .map(|r| {
                                    let (zero, of) = (r.zero(), r.bank.clone());
                                    view! {
                                        <AccountBar
                                            row=r
                                            span
                                            editing
                                            visible=Signal::derive(move || shown(zero, &of))
                                            banks=banks.clone()
                                            today=today.clone()
                                        />
                                    }
                                })
                                .collect_view()}
                        </section>
                    }
                    .into_any()
                })
                .collect_view()}
        </div>
    }
}

/// Month ticks across the bars' span.
#[component]
fn Axis(months: Vec<Tick>, span: Signal<f64>) -> impl IntoView {
    view! {
        <div class="axis">
            <span></span>
            <div class="ticks">
                {move || {
                    let span = span.get();
                    // Every month while they fit, then January only.
                    let yearly = span > 500.0;
                    months
                        .iter()
                        .filter(|t| (t.ago as f64) < span)
                        .filter(|t| !yearly || t.month == 1)
                        .map(|t| {
                            let text = match yearly {
                                true => t.year.to_string(),
                                false => format!("{:02}-{:02}", t.year % 100, t.month),
                            };
                            view! { <span style=format!("left:{:.2}%", at(t.ago, span))>{text}</span> }
                        })
                        .collect_view()
                }}
                <span class="today" style="left:100%">"今仔日"</span>
            </div>
            <span></span>
        </div>
    }
}
