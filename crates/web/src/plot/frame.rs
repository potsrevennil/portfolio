//! What a line and a column chart share: the readout, the axes and grid, a
//! strip per period, and the table.

use leptos::{html, prelude::*};

use super::{
    axis::{centre, labelled, strip, Compact, Scale},
    figure::FigureText,
};
use crate::model::Trend;

/// Wraps a chart's `layer` of marks; `marks` draws the hovered period's.
pub fn frame<M: IntoView + 'static>(
    trend: Trend,
    scale: Scale,
    kind: &'static str,
    layer: AnyView,
    marks: impl Fn(usize) -> M + Send + Sync + 'static,
) -> AnyView {
    let n = trend.periods.len();
    let last = n.saturating_sub(1);
    let hovered = RwSignal::new(None::<usize>);
    // Only a keyboard reader is told each period as it moves: a pointer
    // sweeping the plot would queue one announcement per period crossed.
    let keyed = RwSignal::new(false);
    let shown = move || hovered.get().unwrap_or(last).min(last);
    let area: NodeRef<html::Div> = NodeRef::new();
    let at = move |x: f64| -> Option<usize> {
        let rect = area.get_untracked()?.get_bounding_client_rect();
        let i = ((x - rect.left()) / rect.width() * n as f64).floor();
        (rect.width() > 0.0).then(|| (i.max(0.0) as usize).min(last))
    };
    let key = move |ev: leptos::ev::KeyboardEvent| {
        let now = hovered.get_untracked().unwrap_or(last);
        let next = match ev.key().as_str() {
            "ArrowLeft" => Some(now.saturating_sub(1)),
            "ArrowRight" => Some((now + 1).min(last)),
            "Home" => Some(0),
            "End" => Some(last),
            _ => None,
        };
        if let Some(next) = next {
            ev.prevent_default();
            keyed.set(true);
            hovered.set(Some(next));
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
            let style = strip(i, n);
            view! {
                <div
                    class="band"
                    class:incomplete=incomplete
                    class:hovered=move || hovered.get() == Some(i)
                    style=style
                    title=describe(&trend, i)
                ></div>
            }
        })
        .collect_view();
    let crosshair = move || {
        hovered.get().map(
            |i| view! { <div class="crosshair" style=format!("left:{:.3}%", centre(i, n))></div> },
        )
    };
    let marks = move || hovered.get().map(&marks);
    let readout_trend = StoredValue::new(trend.clone());
    let readout = move || {
        readout_trend.with_value(|t| {
            let i = shown();
            let single = t.series.len() == 1;
            let keys = t
                .series
                .iter()
                .enumerate()
                .map(|(j, s)| {
                    let name = (!single).then(|| {
                        view! {
                            <span class=format!("swatch s{}", j + 1) aria-hidden="true"></span>
                            <span class="name">{s.name.clone()}</span>
                        }
                    });
                    view! {
                        <span class="key">
                            {name}
                            <span class="value"><FigureText figure=s.figures[i].clone() /></span>
                        </span>
                    }
                })
                .collect_view();
            view! {
                <span class="period">{t.periods.get(i).cloned().unwrap_or_default()}</span>
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
                    on:pointermove=move |ev| {
                        keyed.set(false);
                        hovered.set(at(f64::from(ev.client_x())));
                    }
                    on:pointerdown=move |ev| {
                        keyed.set(false);
                        hovered.set(at(f64::from(ev.client_x())));
                    }
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
            <TableView trend />
        </div>
    }
    .into_any()
}

/// One period's figures, for its tooltip.
fn describe(trend: &Trend, i: usize) -> String {
    let figures = trend
        .series
        .iter()
        .chain(&trend.noted)
        .map(|s| {
            let f = &s.figures[i];
            let value = match f.is_complete() {
                true => f.money.to_string(),
                false => format!("未完整{}", f.unpriced),
            };
            format!("{} {value}", s.name)
        })
        .collect::<Vec<_>>()
        .join(" · ");
    format!("{} {figures}", trend.periods[i])
}

/// Every figure a chart draws, as a table: what a reader without the
/// picture, or after an exact number, reads instead.
#[component]
pub fn TableView(trend: Trend) -> AnyView {
    let Trend { periods, series, noted } = trend;
    let columns: Vec<_> = series.into_iter().chain(noted).collect();
    let heads =
        columns.iter().map(|s| view! { <th scope="col">{s.name.clone()}</th> }).collect_view();
    let rows = periods
        .into_iter()
        .enumerate()
        .map(|(i, period)| {
            let cells = columns
                .iter()
                .map(|s| view! { <td><FigureText figure=s.figures[i].clone() /></td> })
                .collect_view();
            view! {
                <tr>
                    <th scope="row">{period}</th>
                    {cells}
                </tr>
            }
        })
        .collect_view();
    view! {
        <details class="table-view">
            <summary>"表格"</summary>
            <div class="scroll">
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"期間"</th>
                            {heads}
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
            </div>
        </details>
    }
    .into_any()
}
