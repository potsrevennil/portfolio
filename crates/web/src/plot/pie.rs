//! A whole and its parts as a donut: the largest parts each in a colour of
//! their own, the rest together as 其他, and a legend giving every part's
//! amount and share of the whole.

use std::f64::consts::TAU;

use leptos::prelude::*;
use rust_decimal::Decimal;

use super::axis::plotted;
use crate::{
    balance_sheet::TotalText,
    model::{account_href, Pie},
};

/// Parts drawn in a colour of their own; a donut of more is hard to read.
const COLOURED: usize = 5;

#[component]
pub fn PieChart(pie: Pie) -> AnyView {
    let Pie { label, total, slices } = pie;
    // Only a whole positive sum takes an angle; the legend still lists the
    // rest.
    let placed: Vec<(usize, f64)> = slices
        .iter()
        .enumerate()
        .filter_map(|(i, s)| plotted(&s.total).filter(|v| *v > 0.0).map(|v| (i, v)))
        .collect();
    let whole: f64 = placed.iter().map(|(_, v)| v).sum();
    // Slot 1–5 for the largest placed parts, 0 (其他) for every other.
    let slot = |i: usize| {
        placed.iter().position(|(j, _)| *j == i).map(|p| match p < COLOURED {
            true => p + 1,
            false => 0,
        })
    };
    let mut drawn: Vec<(usize, f64)> =
        placed.iter().take(COLOURED).enumerate().map(|(p, (_, v))| (p + 1, *v)).collect();
    let rest: f64 = placed.iter().skip(COLOURED).map(|(_, v)| v).sum();
    if rest > 0.0 {
        drawn.push((0, rest));
    }
    let arcs = drawn
        .iter()
        .scan(0.0, |start, &(slot, v)| {
            let from = *start;
            *start += v / whole * TAU;
            Some((slot, from, *start))
        })
        .map(|(slot, from, to)| view! { <path class=format!("slot s{slot}") d=arc(from, to) /> })
        .collect_view();

    let rows = slices
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let swatch = slot(i).map(|n| format!("swatch s{n}"));
            view! {
                <li>
                    <span class=swatch.unwrap_or_else(|| "swatch none".into()) aria-hidden="true"></span>
                    <a class="name" href=account_href(&s.path)>
                        {s.label}
                    </a>
                    <span class="value"><TotalText total=s.total /></span>
                    <span class="part">{s.part.map(percent)}</span>
                </li>
            }
        })
        .collect_view();
    let empty = (whole <= 0.0).then(|| view! { <p class="note">"無"</p> });
    view! {
        <div class="pie">
            <div class="donut">
                <svg viewBox="0 0 100 100" aria-hidden="true">{arcs}</svg>
                <div class="centre">
                    <span class="name">{label}</span>
                    <span class="value"><TotalText total /></span>
                </div>
            </div>
            {empty}
            <ul class="legend-list">{rows}</ul>
        </div>
    }
    .into_any()
}

/// A part of the whole as a reader takes it in: whole percents, and a part
/// too small to round to one still shown as there.
fn percent(part: Decimal) -> String {
    let hundredths = (part * Decimal::ONE_HUNDRED).round();
    match (hundredths.is_zero(), part > Decimal::ZERO) {
        (true, true) => "<1%".into(),
        _ => format!("{hundredths}%"),
    }
}

/// The ring segment from `from` to `to` radians, clockwise from the top.
fn arc(from: f64, to: f64) -> String {
    const OUTER: f64 = 48.0;
    const INNER: f64 = 30.0;
    let at = |r: f64, a: f64| (50.0 + r * a.sin(), 50.0 - r * a.cos());
    match to - from >= TAU - 1e-9 {
        // A whole ring has no ends: two half circles each way.
        true => format!(
            "M50,{o0}A{OUTER},{OUTER} 0 1 1 50,{o1}A{OUTER},{OUTER} 0 1 1 \
             50,{o0}ZM50,{i0}A{INNER},{INNER} 0 1 0 50,{i1}A{INNER},{INNER} 0 1 0 50,{i0}Z",
            o0 = 50.0 - OUTER,
            o1 = 50.0 + OUTER,
            i0 = 50.0 - INNER,
            i1 = 50.0 + INNER,
        ),
        false => {
            let large = u8::from(to - from > TAU / 2.0);
            let (ox0, oy0) = at(OUTER, from);
            let (ox1, oy1) = at(OUTER, to);
            let (ix1, iy1) = at(INNER, to);
            let (ix0, iy0) = at(INNER, from);
            format!(
                "M{ox0:.3},{oy0:.3}A{OUTER},{OUTER} 0 {large} 1 \
                 {ox1:.3},{oy1:.3}L{ix1:.3},{iy1:.3}A{INNER},{INNER} 0 {large} 0 \
                 {ix0:.3},{iy0:.3}Z"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn parts_read_in_whole_percents() {
        assert_eq!(percent(dec!(0.9375)), "94%");
        assert_eq!(percent(dec!(0.004)), "<1%");
        assert_eq!(percent(Decimal::ZERO), "0%");
        assert_eq!(percent(Decimal::ONE), "100%");
    }

    #[test]
    fn a_quarter_from_the_top_ends_at_three_o_clock() {
        assert_eq!(
            arc(0.0, TAU / 4.0),
            "M50.000,2.000A48,48 0 0 1 98.000,50.000L80.000,50.000A30,30 0 0 0 50.000,20.000Z"
        );
        // More than half takes the long way round.
        assert!(arc(0.0, TAU * 0.75).contains(" 0 1 1 "));
    }
}
