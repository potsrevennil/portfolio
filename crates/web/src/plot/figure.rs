//! A figure as a chart's readout and tables show it.

use leptos::prelude::*;

use crate::{
    balance_sheet::{MoneyText, UnpricedNote},
    model::Converted,
};

/// The figure, or 未完整 and what was left out: a sum missing an amount is
/// never shown as the total.
#[component]
pub fn FigureText(figure: Converted) -> impl IntoView {
    match figure.is_complete() {
        true => view! { <MoneyText money=figure.money /> }.into_any(),
        false => view! {
            <span class="incomplete">"未完整"</span>
            <UnpricedNote unpriced=figure.unpriced />
        }
        .into_any(),
    }
}
