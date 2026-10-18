//! Bars for parts of a whole.

use leptos::prelude::*;

use super::{axis::plotted, figure::FigureText};
use crate::model::Share;

/// Where a flow came from or went: a bar per account, largest first, its
/// sub-accounts folded beneath it. Bars share one scale, the largest
/// top-level account's; each figure also says its part of the whole.
#[component]
pub fn ShareBars(
    share: Share,
    /// The series colour the bars take, so they match the columns above.
    hue: usize,
) -> AnyView {
    let largest = share.children.iter().filter_map(|s| plotted(&s.total)).fold(0.0_f64, f64::max);
    let whole = plotted(&share.total).filter(|w| *w > 0.0);
    let rows =
        share.children.into_iter().map(|s| share_row(s, 0, largest, whole, hue)).collect_view();
    view! {
        <div class="shares">
            <div class="share whole">
                <span class="name">{share.label}</span>
                <span class="value"><FigureText figure=share.total /></span>
            </div>
            {rows}
        </div>
    }
    .into_any()
}

fn share_row(s: Share, depth: usize, largest: f64, whole: Option<f64>, hue: usize) -> AnyView {
    let value = plotted(&s.total);
    let width = value.filter(|_| largest > 0.0).map(|v| (v / largest * 100.0).clamp(0.0, 100.0));
    let part = value.zip(whole).map(|(v, w)| format!("{:.0}%", v / w * 100.0));
    let row = view! {
        <span class="marker" aria-hidden="true"></span>
        <span class="name">{s.label}</span>
        <span class="track" aria-hidden="true">
            {width.map(|w| view! { <span class=format!("fill s{hue}") style=format!("width:{w:.2}%")></span> })}
        </span>
        <span class="value">
            <FigureText figure=s.total />
            {part.map(|p| view! { <span class="part">{p}</span> })}
        </span>
    };
    let indent = format!("padding-left: calc({depth} * 1.1rem + 0.2rem)");
    match s.children.is_empty() {
        true => view! { <div class="share" style=indent>{row}</div> }.into_any(),
        false => {
            let children = s
                .children
                .into_iter()
                .map(|c| share_row(c, depth + 1, largest, whole, hue))
                .collect_view();
            view! {
                <details class="group">
                    <summary class="share" style=indent>{row}</summary>
                    {children}
                </details>
            }
            .into_any()
        }
    }
}
