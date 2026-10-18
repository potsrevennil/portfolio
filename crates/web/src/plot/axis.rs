//! Where things go on a chart: the value scale, the periods across, and
//! how axis figures read.

use std::fmt;

use rust_decimal::prelude::ToPrimitive;

use crate::model::{Converted, Trend};

/// Every series' figures, those it can place, on one scale.
pub struct Plot {
    /// Per series, per period; none for a figure missing an amount.
    pub values: Vec<Vec<Option<f64>>>,
    pub scale: Scale,
}

impl From<&Trend> for Plot {
    fn from(trend: &Trend) -> Self {
        let values: Vec<Vec<Option<f64>>> =
            trend.series.iter().map(|s| s.figures.iter().map(plotted).collect()).collect();
        let scale = values.iter().flatten().flatten().copied().collect();
        Plot { values, scale }
    }
}

/// A value axis on round steps, always taking in zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    pub lo: f64,
    pub hi: f64,
    pub ticks: Vec<f64>,
}

impl FromIterator<f64> for Scale {
    fn from_iter<I: IntoIterator<Item = f64>>(values: I) -> Scale {
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
}

impl Scale {
    /// How far down the plot `v` sits, in percent.
    pub fn top(&self, v: f64) -> f64 { (self.hi - v) / (self.hi - self.lo) * 100.0 }
}

/// An axis figure: grouped below 一萬, then in 萬 or 億 to one decimal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Compact(pub f64);

impl fmt::Display for Compact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        let decimals_of = |unit: &str| -> usize {
            match (unit, v.fract() == 0.0) {
                ("", true) => 0,
                ("", false) => 2,
                _ => 1,
            }
        };
        // The largest unit the value still fills once rounded as that unit
        // shows it, so 99,999,600 reads 1億, not 10000萬.
        let (unit, size) = [("億", 1e8), ("萬", 1e4)]
            .into_iter()
            .find(|(unit, size)| {
                let scale = 10_f64.powi(decimals_of(unit) as i32);
                (v.abs() / size * scale).round() / scale >= 1.0
            })
            .unwrap_or(("", 1.0));
        let decimals = decimals_of(unit);
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
            Some(frac) => write!(f, "{sign}{grouped}.{frac}{unit}"),
            None => write!(f, "{sign}{grouped}{unit}"),
        }
    }
}

/// A figure the chart can place: none for one missing an amount.
pub fn plotted(figure: &Converted) -> Option<f64> {
    figure.is_complete().then(|| figure.money.amount.to_f64()).flatten()
}

/// The middle of period `i` of `n`, in percent across.
pub fn centre(i: usize, n: usize) -> f64 { (i as f64 + 0.5) / n as f64 * 100.0 }

/// Which periods get an axis label: about six, counted back from the newest.
pub fn labelled(i: usize, n: usize) -> bool { (n - 1 - i).is_multiple_of(n.div_ceil(6).max(1)) }

/// Period `i` of `n` as a strip across the plot.
pub fn strip(i: usize, n: usize) -> String {
    format!("left:{:.3}%;width:{:.3}%", i as f64 / n as f64 * 100.0, 100.0 / n as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_figures_are_compact_in_wan_and_yi() {
        let cases = [
            (0.0, "0"),
            (-0.0, "0"),
            (2500.0, "2,500"),
            (-500.0, "-500"),
            (0.5, "0.5"),
            (10000.0, "1萬"),
            (12000.0, "1.2萬"),
            (-25000.0, "-2.5萬"),
            (1234500.0, "123.5萬"),
            (150000000.0, "1.5億"),
            (99999600.0, "1億"),
            (9999.96, "1萬"),
        ];
        for (v, text) in cases {
            assert_eq!(Compact(v).to_string(), text, "{v}");
        }
    }

    #[test]
    fn the_axis_takes_in_zero_on_round_steps() {
        let scale = Scale::from_iter([39500.0, -500.0]);
        assert_eq!(scale.ticks, [-10000.0, 0.0, 10000.0, 20000.0, 30000.0, 40000.0]);
        assert_eq!(scale.top(40000.0), 0.0);
        assert_eq!(scale.top(-10000.0), 100.0);
        // Nothing to draw still makes an axis.
        assert_eq!(Scale::from_iter([]).ticks, [0.0, 1.0]);
        let small = Scale::from_iter([3.0, 7.5]);
        assert_eq!((small.lo, small.hi), (0.0, 8.0));
    }
}
