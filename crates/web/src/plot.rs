//! Charts drawn on the server: SVG lines over an HTML frame, HTML columns and
//! bars, all sized in percentages so they stretch to any width with no
//! script. Once hydrated, a readout follows the pointer or the arrow keys
//! across the periods; before that, each period's figures are its tooltip,
//! and every chart has its table.

use leptos::{html, prelude::*};
use rust_decimal::prelude::ToPrimitive;

use crate::{
    balance_sheet::{MoneyText, UnpricedNote},
    model::{Converted, Share, Trend},
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

/// A value axis on round steps, always taking in zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    pub lo: f64,
    pub hi: f64,
    pub ticks: Vec<f64>,
}

impl Scale {
    pub fn over(values: impl IntoIterator<Item = f64>) -> Scale {
        let (lo, hi) =
            values.into_iter().fold((0.0_f64, 0.0_f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let rough = if hi > lo { (hi - lo) / 4.0 } else { 1.0 };
        let magnitude = 10_f64.powf(rough.log10().floor());
        let step = [1.0, 2.0, 2.5, 5.0, 10.0]
            .into_iter()
            .map(|m| m * magnitude)
            .find(|s| *s >= rough)
            .unwrap_or(10.0 * magnitude);
        let lo = (lo / step).floor() * step;
        let hi = ((hi / step).ceil() * step).max(lo + step);
        let steps = ((hi - lo) / step).round() as usize;
        Scale { lo, hi, ticks: (0..=steps).map(|i| lo + step * i as f64).collect() }
    }

    /// How far down the plot `v` sits, in percent.
    pub fn top(&self, v: f64) -> f64 { (self.hi - v) / (self.hi - self.lo) * 100.0 }
}

/// An axis figure: grouped below 一萬, then in 萬 or 億 to one decimal.
pub fn compact(v: f64) -> String {
    let (unit, size) = match v.abs() {
        a if a >= 1e8 => ("億", 1e8),
        a if a >= 1e4 => ("萬", 1e4),
        _ => ("", 1.0),
    };
    let decimals = match (unit, v.fract() == 0.0) {
        ("", true) => 0,
        ("", false) => 2,
        _ => 1,
    };
    let text = format!("{:.*}", decimals, v / size);
    let text = match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').to_string(),
        false => text,
    };
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) if rest.chars().any(|c| c != '0' && c != '.') => ("-", rest),
        Some(rest) => ("", rest),
        None => ("", text.as_str()),
    };
    let (int, frac) = digits.split_once('.').map_or((digits, None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, digit) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    match frac {
        Some(frac) => format!("{sign}{grouped}.{frac}{unit}"),
        None => format!("{sign}{grouped}{unit}"),
    }
}

/// A figure the chart can place: none for one missing an amount.
fn plotted(figure: &Converted) -> Option<f64> {
    figure.is_complete().then(|| figure.money.amount.to_f64()).flatten()
}

/// The middle of period `i` of `n`, in percent across.
fn centre(i: usize, n: usize) -> f64 { (i as f64 + 0.5) / n as f64 * 100.0 }

/// Which periods get an axis label: about six, counted back from the newest.
fn labelled(i: usize, n: usize) -> bool { (n - 1 - i).is_multiple_of(n.div_ceil(6).max(1)) }

/// Net worth and the like: one line per series, broken where a figure is
/// missing an amount.
#[component]
pub fn LineChart(trend: Trend) -> AnyView {
    let n = trend.periods.len();
    let values: Vec<Vec<Option<f64>>> =
        trend.series.iter().map(|s| s.figures.iter().map(plotted).collect()).collect();
    let scale = Scale::over(values.iter().flatten().flatten().copied());
    let lines = values
        .iter()
        .enumerate()
        .map(|(j, points)| {
            let mut d = String::new();
            let mut drawing = false;
            for (i, v) in points.iter().enumerate() {
                match v {
                    Some(v) => {
                        let command = if drawing { 'L' } else { 'M' };
                        let (x, y) = (centre(i, n) * 10.0, scale.top(*v) * 10.0);
                        d.push_str(&format!("{command}{x:.1},{y:.1}"));
                        drawing = true;
                    }
                    None => drawing = false,
                }
            }
            view! { <path class=format!("line s{}", j + 1) d=d vector-effect="non-scaling-stroke" /> }
        })
        .collect_view();
    // A point with no neighbour draws no line, so it gets a dot of its own.
    let lone = values
        .iter()
        .enumerate()
        .flat_map(|(j, points)| {
            let at = |i: Option<usize>| i.and_then(|i| points.get(i)).copied().flatten();
            points
                .iter()
                .enumerate()
                .filter(move |(i, v)| {
                    v.is_some() && at(i.checked_sub(1)).is_none() && at(Some(i + 1)).is_none()
                })
                .map(move |(i, v)| (j, i, v.unwrap_or_default()))
                .collect::<Vec<_>>()
        })
        .map(|(j, i, v)| {
            let style = format!("left:{:.3}%;top:{:.3}%", centre(i, n), scale.top(v));
            view! { <span class=format!("dot s{}", j + 1) style=style></span> }
        })
        .collect_view();
    let marks = {
        let scale = scale.clone();
        move |shown: usize| {
            values
                .iter()
                .enumerate()
                .filter_map(|(j, points)| {
                    let v = points.get(shown).copied().flatten()?;
                    let style = format!("left:{:.3}%;top:{:.3}%", centre(shown, n), scale.top(v));
                    Some(view! { <span class=format!("dot s{}", j + 1) style=style></span> })
                })
                .collect_view()
        }
    };
    let layer = view! {
        <svg class="lines" viewBox="0 0 1000 1000" preserveAspectRatio="none" aria-hidden="true">
            {lines}
        </svg>
        {lone}
    };
    frame(trend, scale, "line", layer.into_any(), marks)
}

/// Income against expense: a column per series in each period, up or down
/// from zero.
#[component]
pub fn ColumnChart(trend: Trend) -> AnyView {
    let n = trend.periods.len();
    let values: Vec<Vec<Option<f64>>> =
        trend.series.iter().map(|s| s.figures.iter().map(plotted).collect()).collect();
    let scale = Scale::over(values.iter().flatten().flatten().copied());
    let zero = scale.top(0.0);
    let columns = (0..n)
        .map(|i| {
            let style =
                format!("left:{:.3}%;width:{:.3}%", i as f64 / n as f64 * 100.0, 100.0 / n as f64);
            let bars = values
                .iter()
                .enumerate()
                .map(|(j, points)| {
                    let fill = points.get(i).copied().flatten().map(|v| {
                        let top = scale.top(v);
                        let (from, height, way) = match v >= 0.0 {
                            true => (top, zero - top, "up"),
                            false => (zero, top - zero, "down"),
                        };
                        let style = format!("top:{from:.3}%;height:{height:.3}%");
                        view! { <span class=format!("fill s{} {way}", j + 1) style=style></span> }
                    });
                    view! { <span class="col">{fill}</span> }
                })
                .collect_view();
            view! { <div class="cols" style=style>{bars}</div> }
        })
        .collect_view();
    frame(trend, scale, "column", columns.into_any(), |_| ())
}

/// The parts a line and a column chart share: the readout, the axes and
/// grid, a band per period, and the table.
fn frame<M: IntoView + 'static>(
    trend: Trend,
    scale: Scale,
    kind: &'static str,
    layer: AnyView,
    marks: impl Fn(usize) -> M + Send + Sync + 'static,
) -> AnyView {
    let n = trend.periods.len();
    let last = n.saturating_sub(1);
    let hovered = RwSignal::new(None::<usize>);
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
        .map(|&t| view! { <span style=format!("top:{:.3}%", scale.top(t))>{compact(t)}</span> })
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
            let style =
                format!("left:{:.3}%;width:{:.3}%", i as f64 / n as f64 * 100.0, 100.0 / n as f64);
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
            <div class="readout" aria-live="polite">{readout}</div>
            <div class="plot">
                <div class="y-axis" aria-hidden="true">{y_labels}</div>
                <div
                    class="area"
                    node_ref=area
                    tabindex="0"
                    aria-label="用左右鍵看每期"
                    on:pointermove=move |ev| hovered.set(at(f64::from(ev.client_x())))
                    on:pointerdown=move |ev| hovered.set(at(f64::from(ev.client_x())))
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
