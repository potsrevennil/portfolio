//! The landing page: the few figures worth seeing first, each linking to the
//! page that explains it. Charts join it with the next page.

use leptos::prelude::*;
use leptos_meta::Title;

use crate::{
    balance_sheet::{load_balance_sheet, MoneyText, UnpricedNote},
    error::LoadFailed,
    model::{BalanceSheet, Converted},
};

#[component]
pub fn SummaryPage() -> impl IntoView {
    let sheet = Resource::new(|| (), |_| load_balance_sheet());
    view! {
        <Title text="總覽" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match sheet.await {
                    Ok(sheet) => view! { <SummaryView sheet /> }.into_any(),
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn SummaryView(sheet: BalanceSheet) -> impl IntoView {
    let sections: Vec<(String, Converted)> =
        sheet.sections.into_iter().map(|s| (s.label, s.total)).collect();
    view! {
        <div class="summary">
            <p class="as-of">{sheet.as_of}</p>
            <div class="headline">
                <span class="name">"淨資產"</span>
                <MoneyText money=sheet.net_worth.money.clone() />
                <UnpricedNote unpriced=sheet.net_worth.unpriced />
            </div>
            <div class="tiles">
                {sections.into_iter().map(|(label, total)| view! { <Tile label total /> }).collect_view()}
            </div>
            // Apart from the tiles above: no total includes it.
            {sheet
                .at_cost
                .map(|s| {
                    view! {
                        <div class="tiles aside">
                            <Tile label=s.label total=s.total />
                        </div>
                    }
                })}
        </div>
    }
}

/// A heading over its total.
#[component]
fn Tile(label: String, total: Converted) -> impl IntoView {
    view! {
        <div class="tile">
            <span class="name">{label}</span>
            <MoneyText money=total.money />
            <UnpricedNote unpriced=total.unpriced />
        </div>
    }
}
