//! Date and span shorthands shared by this crate's unit tests.

use chrono::{Datelike, NaiveDate};

use crate::span::Span;

pub fn date(s: &str) -> NaiveDate { s.parse().unwrap() }

pub fn span(from: &str, through: &str) -> Span { Span::new(date(from), date(through)) }

/// The days FX trades across `span`.
pub fn weekdays(span: Span) -> Vec<NaiveDate> {
    span.into_iter().filter(|day| day.weekday().number_from_monday() <= 5).collect()
}
