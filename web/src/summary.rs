//! The landing page: the few figures worth seeing first, each linking to the
//! page that explains it. Charts join it with the next page.

use leptos::prelude::*;
use leptos_meta::Title;

use crate::{
    balance_sheet::{load_balance_sheet, Excluded, MoneyText, Unpriced},
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
                    Err(e) => view! { <p class="note error">{format!("讀取失敗：{e}")}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn SummaryView(sheet: BalanceSheet) -> impl IntoView {
    let sections: Vec<(String, Converted)> =
        sheet.sections.into_iter().map(|s| (s.label, s.converted.unwrap_or(s.total))).collect();
    view! {
        <div class="summary">
            <p class="as-of">{sheet.as_of}</p>
            <div class="headline">
                <span class="name">"淨資產"</span>
                <MoneyText money=sheet.net_worth.money.clone() />
                <Unpriced currencies=sheet.net_worth.unpriced />
                <Excluded excluded=sheet.excluded />
            </div>
            <div class="tiles">
                {sections
                    .into_iter()
                    .map(|(label, total)| {
                        view! {
                            <div class="tile">
                                <span class="name">{label}</span>
                                <MoneyText money=total.money />
                                <Unpriced currencies=total.unpriced />
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}
