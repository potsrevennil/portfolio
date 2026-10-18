//! How much of each needed currency the cache can price, and the gaps left.

use std::{collections::HashMap, fmt};

use chrono::NaiveDate;
use ledger_types::Currency;

use super::route::rated;
use crate::span::{uncovered, Span, SLACK};

/// A currency the ledger holds, from its first posting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub currency: Currency,
    pub since: NaiveDate,
}

/// Days a currency has no rate for and the source was never asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub currency: Currency,
    pub span: Span,
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.currency, self.span)
    }
}

/// How well one currency can be priced in the base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub currency: Currency,
    /// From the first posting to today.
    pub need: Span,
    /// The days some route has a quote for.
    pub rated: Vec<Span>,
    /// What to ask the source for, oldest first.
    pub gaps: Vec<Span>,
}

impl Coverage {
    /// No quote on any route: every amount in it stays unconverted.
    pub fn unrated(&self) -> bool { self.rated.is_empty() }

    pub fn gaps(&self) -> impl Iterator<Item = Gap> + '_ {
        self.gaps.iter().map(|&span| Gap { currency: self.currency, span })
    }
}

/// Each needed currency's coverage. Held = the days quoted plus the days
/// already asked of the source (so a pair it has no history for is not asked
/// again every run); a gap is any unheld stretch longer than a holiday, or any
/// unheld tail up to today. A tail reaches back to the newest quote, so a close
/// fetched before it was final is fetched again.
pub fn coverage(
    needs: &[Need],
    base: Currency,
    today: NaiveDate,
    quotes: &HashMap<String, Vec<NaiveDate>>,
    asked: &HashMap<Currency, Vec<Span>>,
) -> Vec<Coverage> {
    needs
        .iter()
        .filter(|n| n.currency != base && n.since <= today)
        .map(|n| {
            let need = Span::new(n.since, today);
            let rated = rated(n.currency, base, quotes);
            let held: Vec<Span> =
                rated.iter().chain(asked.get(&n.currency).into_iter().flatten()).copied().collect();
            let newest = rated.last().map(|r| r.through);
            let gaps = uncovered(need, &held)
                .into_iter()
                .filter(|g| g.through == today || g.days() > SLACK)
                .map(|g| match newest {
                    Some(newest) if g.through == today && newest < g.from => {
                        Span::new(newest, g.through)
                    }
                    _ => g,
                })
                .collect();
            Coverage { currency: n.currency, need, rated, gaps }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{date, span, weekdays};

    fn need(currency: Currency, since: &str) -> Need { Need { currency, since: date(since) } }

    /// JPY's coverage in TWD, needed from `since`.
    fn jpy(
        since: &str,
        today: &str,
        quotes: &[(&str, Vec<NaiveDate>)],
        asked: &[Span],
    ) -> Coverage {
        let quotes = quotes.iter().map(|(t, q)| (t.to_string(), q.clone())).collect();
        let asked = HashMap::from([(Currency::JPY, asked.to_vec())]);
        coverage(&[need(Currency::JPY, since)], Currency::TWD, date(today), &quotes, &asked)
            .remove(0)
    }

    #[test]
    fn a_currency_with_no_quotes_is_unrated_from_its_first_posting() {
        let got = coverage(
            &[need(Currency::JPY, "2024-01-10"), need(Currency::TWD, "2020-01-01")],
            Currency::TWD,
            date("2024-03-01"),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(got.len(), 1, "the base needs no rate");
        assert!(got[0].unrated());
        assert_eq!(got[0].gaps, vec![span("2024-01-10", "2024-03-01")]);
    }

    #[test]
    fn gaps_are_the_head_the_holes_and_the_tail() {
        let quoted = [
            weekdays(span("2024-02-01", "2024-02-29")),
            weekdays(span("2024-04-01", "2024-04-30")),
        ]
        .concat();
        let got = jpy("2024-01-01", "2024-05-06", &[("JPYTWD=X", quoted)], &[]);
        assert_eq!(got.gaps, vec![
            span("2024-01-01", "2024-01-31"),
            span("2024-03-01", "2024-03-31"),
            // A tail counts at any length, from the newest quote, which may
            // have been fetched before the close.
            span("2024-04-30", "2024-05-06"),
        ]);
    }

    #[test]
    fn weekends_and_short_head_holes_are_not_gaps() {
        let quoted = weekdays(span("2024-01-03", "2024-01-31"));
        let got = jpy("2024-01-01", "2024-01-31", &[("JPYTWD=X", quoted)], &[]);
        assert_eq!(got.gaps, vec![]);
    }

    #[test]
    fn days_already_asked_are_not_asked_again() {
        let got = jpy("2024-01-01", "2024-03-01", &[], &[span("2024-01-01", "2024-02-15")]);
        assert!(got.unrated(), "asking is not a rate");
        assert_eq!(got.gaps, vec![span("2024-02-16", "2024-03-01")]);
    }
}
