//! Lines, for figures that carry on from one period to the next.

use leptos::prelude::*;

use super::{
    axis::{centre, Plot},
    frame::frame,
};
use crate::model::Trend;

/// Net worth and the like: one line per series, broken where a figure is
/// missing an amount.
#[component]
pub fn LineChart(trend: Trend) -> AnyView {
    let n = trend.periods.len();
    let Plot { values, scale } = Plot::from(&trend);
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
    frame(trend, scale, "line", layer.into_any(), marks, None, Vec::new())
}
