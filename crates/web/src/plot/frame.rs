//! What a line and a column chart share: the readout, the axes and grid,
//! and a strip per period, which may link to that period.

use leptos::{html, prelude::*, wasm_bindgen::JsCast};

use super::axis::{centre, labelled, strip, Compact, Scale};
use crate::{balance_sheet::TotalText, model::Trend};

/// Wraps a chart's `layer` of marks; `marks` draws the hovered period's.
pub fn frame<M: IntoView + 'static>(
    trend: Trend,
    scale: Scale,
    kind: &'static str,
    layer: AnyView,
    marks: impl Fn(usize) -> M + Send + Sync + 'static,
    // Where picking each period leads, if anywhere.
    links: Option<Vec<String>>,
    // The periods shown elsewhere on the page, marked.
    marked: Vec<usize>,
) -> AnyView {
    let n = trend.periods.len();
    let last = n.saturating_sub(1);
    let hovered = RwSignal::new(None::<usize>);
    // Only a keyboard reader is told each period as it moves: a pointer
    // sweeping the plot would queue one announcement per period crossed.
    let keyed = RwSignal::new(false);
    let area: NodeRef<html::Div> = NodeRef::new();
    let at = move |x: f64| -> Option<usize> {
        let rect = area.get_untracked()?.get_bounding_client_rect();
        let i = ((x - rect.left()) / rect.width() * n as f64).floor();
        (rect.width() > 0.0).then(|| (i.max(0.0) as usize).min(last))
    };
    let links = StoredValue::new(links);
    /// What a key does on the plot.
    enum Move {
        To(usize),
        Open(usize),
        Nothing,
    }
    // A pointer redraws only when it crosses into another period.
    let point = move |x: f64| {
        if keyed.get_untracked() {
            keyed.set(false);
        }
        let under = at(x);
        if hovered.get_untracked() != under {
            hovered.set(under);
        }
    };
    let key = move |ev: leptos::ev::KeyboardEvent| {
        let now = hovered.get_untracked().unwrap_or(last);
        let action = match ev.key().as_str() {
            "ArrowLeft" => Move::To(now.saturating_sub(1)),
            "ArrowRight" => Move::To((now + 1).min(last)),
            "Home" => Move::To(0),
            "End" => Move::To(last),
            "Enter" if links.with_value(Option::is_some) => Move::Open(now),
            _ => Move::Nothing,
        };
        match action {
            Move::To(next) => {
                ev.prevent_default();
                keyed.set(true);
                hovered.set(Some(next));
            }
            Move::Open(i) => {
                ev.prevent_default();
                // A press on the period's own link, so the router follows it
                // as it follows a pointer's, page kept in place.
                let link = area
                    .get_untracked()
                    .and_then(|a| a.query_selector(&format!("a.band:nth-of-type({})", i + 1)).ok())
                    .flatten()
                    .and_then(|link| link.dyn_into::<leptos::web_sys::HtmlElement>().ok());
                if let Some(link) = link {
                    link.click();
                }
            }
            Move::Nothing => {}
        }
    };

    let grid = scale
        .ticks
        .iter()
        .map(|&t| {
            let y = scale.top(t) * 10.0;
            let class = if t == 0.0 { "zero" } else { "grid" };
            view! {
                <line class=class x1="0" x2="1000" y1=format!("{y:.1}") y2=format!("{y:.1}") vector-effect="non-scaling-stroke" />
            }
        })
        .collect_view();
    let y_labels = scale
        .ticks
        .iter()
        .map(|&t| view! { <span style=format!("top:{:.3}%", scale.top(t))>{Compact(t).to_string()}</span> })
        .collect_view();
    // Every other label gives way on a narrow screen.
    let x_labels = (0..n)
        .filter(|&i| labelled(i, n))
        .map(|i| {
            let alternate = (n - 1 - i) / n.div_ceil(6).max(1) % 2 == 1;
            view! {
                <span class:alternate=alternate style=format!("left:{:.3}%", centre(i, n))>
                    {trend.periods[i].clone()}
                </span>
            }
        })
        .collect_view();
    let bands = (0..n)
        .map(|i| {
            let incomplete = trend.series.iter().any(|s| !s.figures[i].is_complete());
            let class = format!(
                "band{}{}",
                if incomplete { " incomplete" } else { "" },
                if marked.contains(&i) { " selected" } else { "" }
            );
            let hovered = move || hovered.get() == Some(i);
            // A link per period is reached by the plot's own keys, not by
            // tabbing through every period.
            match links.with_value(|l| l.as_ref().map(|l| l[i].clone())) {
                Some(href) => view! {
                    <a
                        class=class
                        class:hovered=hovered
                        style=strip(i, n)
                        href=href
                        tabindex="-1"
                        data-noscroll="true"
                    ></a>
                }
                .into_any(),
                None => view! { <div class=class class:hovered=hovered style=strip(i, n)></div> }
                    .into_any(),
            }
        })
        .collect_view();
    let crosshair = move || {
        hovered.get().map(
            |i| view! { <div class="crosshair" style=format!("left:{:.3}%", centre(i, n))></div> },
        )
    };
    let marks = move || hovered.get().map(&marks);
    let readout_trend = StoredValue::new(trend);
    let readout = move || {
        readout_trend.with_value(|t| {
            // A lone series needs no key unless figures are read out beside it.
            let named = t.series.len() > 1 || !t.noted.is_empty();
            let swatched = t.series.len() > 1;
            let names = t.series.iter().chain(&t.noted).map(|s| s.name.clone());
            // Until a period is picked, the summary stands for the whole.
            let (period, figures): (String, Vec<_>) = match (hovered.get(), &t.summary) {
                (None, Some(summary)) => (summary.name.clone(), summary.figures.clone()),
                (picked, _) => {
                    let i = picked.unwrap_or(last).min(last);
                    let figures = t.series.iter().chain(&t.noted).map(|s| s.figures[i].clone());
                    (t.periods.get(i).cloned().unwrap_or_default(), figures.collect())
                }
            };
            let keys = names
                .zip(figures)
                .enumerate()
                .map(|(j, (name, figure))| {
                    let swatch = (swatched && j < t.series.len()).then(|| {
                        view! { <span class=format!("swatch s{}", j + 1) aria-hidden="true"></span> }
                    });
                    let name = named.then(|| view! { <span class="name">{name}</span> });
                    view! {
                        <span class="key">
                            {swatch}
                            {name}
                            <span class="value"><TotalText total=figure /></span>
                        </span>
                    }
                })
                .collect_view();
            view! {
                <span class="period">{period}</span>
                {keys}
            }
        })
    };

    view! {
        <div class=format!("chart {kind}")>
            <div class="readout" aria-live=move || if keyed.get() { "polite" } else { "off" }>
                {readout}
            </div>
            <div class="plot">
                <div class="y-axis" aria-hidden="true">{y_labels}</div>
                <div
                    class="area"
                    node_ref=area
                    tabindex="0"
                    aria-label="用左右鍵看每期"
                    on:pointermove=move |ev| point(f64::from(ev.client_x()))
                    on:pointerdown=move |ev| point(f64::from(ev.client_x()))
                    on:pointerleave=move |_| hovered.set(None)
                    // A press focuses the plot after placing the readout;
                    // focus only starts one that is not placed yet.
                    on:focus=move |_| {
                        if hovered.get_untracked().is_none() {
                            hovered.set(Some(last));
                        }
                    }
                    on:blur=move |_| hovered.set(None)
                    on:keydown=key
                >
                    <svg class="grid" viewBox="0 0 1000 1000" preserveAspectRatio="none" aria-hidden="true">
                        {grid}
                    </svg>
                    {bands}
                    {layer}
                    {crosshair}
                    {marks}
                </div>
                <div class="x-axis" aria-hidden="true">{x_labels}</div>
            </div>
        </div>
    }
    .into_any()
}
