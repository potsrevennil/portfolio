//! Columns, for what moved within each period.

use leptos::prelude::*;

use super::{
    axis::{strip, Plot},
    frame::frame,
};
use crate::model::Trend;

/// Income against expense: a column per series in each period, up or down
/// from zero.
#[component]
pub fn ColumnChart(trend: Trend) -> AnyView {
    let n = trend.periods.len();
    let Plot { values, scale } = Plot::from(&trend);
    let zero = scale.top(0.0);
    let columns = (0..n)
        .map(|i| {
            let style = strip(i, n);
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
