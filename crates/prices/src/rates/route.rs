//! The ways to price one currency in another, and which days the quotes held
//! can price it on.

use std::collections::HashMap;

use chrono::NaiveDate;
use ledger_types::Currency;

use crate::{
    source::YFinanceSource,
    span::{covered, intersect, union, Span},
};

/// One conversion step, read from either pair's quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    pub from: Currency,
    pub to: Currency,
}

impl Leg {
    /// The pair quoting `to` per `from`, then its inverse.
    pub fn tickers(&self) -> [String; 2] {
        [YFinanceSource::ticker(self.from, self.to), YFinanceSource::ticker(self.to, self.from)]
    }
}

/// The ways to price `from` in `to`, best first: directly, or through USD,
/// which Yahoo quotes against nearly everything.
pub fn routes(from: Currency, to: Currency) -> Vec<Vec<Leg>> {
    let direct = vec![Leg { from, to }];
    match (from == to, from == Currency::USD || to == Currency::USD) {
        (true, _) => Vec::new(),
        (false, true) => vec![direct],
        (false, false) => {
            vec![direct, vec![Leg { from, to: Currency::USD }, Leg { from: Currency::USD, to }]]
        }
    }
}

/// Every ticker a route to price `from` in `to` reads.
pub fn tickers(from: Currency, to: Currency) -> Vec<String> {
    routes(from, to).iter().flatten().flat_map(Leg::tickers).collect()
}

/// What to ask the source for, in turn, until a gap is priced: the pair, its
/// inverse (thin pairs are often quoted only the other way round), then both
/// legs through USD. The step whose quotes reach furthest goes first: the
/// route that prices a currency now is the one to keep current.
pub(super) fn fetch_plan(
    from: Currency,
    to: Currency,
    quotes: &HashMap<String, Vec<NaiveDate>>,
) -> Vec<Vec<String>> {
    let ticker = YFinanceSource::ticker;
    let pairs = vec![vec![ticker(from, to)], vec![ticker(to, from)]];
    let mut plan = match routes(from, to).len() > 1 {
        true => {
            [pairs, vec![vec![ticker(Currency::USD, from), ticker(Currency::USD, to)]]].concat()
        }
        false => pairs,
    };
    // A step reaches as far as its stalest ticker; one with no quotes, nowhere.
    let reach = |step: &Vec<String>| {
        step.iter()
            .map(|t| quotes.get(t).and_then(|q| q.iter().max().copied()))
            .collect::<Option<Vec<_>>>()
            .and_then(|newest| newest.into_iter().min())
    };
    plan.sort_by_key(|step| std::cmp::Reverse(reach(step)));
    plan
}

/// The days `from` can be priced in `to` from the quotes held.
pub fn rated(from: Currency, to: Currency, quotes: &HashMap<String, Vec<NaiveDate>>) -> Vec<Span> {
    let leg = |leg: &Leg| {
        covered(leg.tickers().iter().flat_map(|t| quotes.get(t).into_iter().flatten().copied()))
    };
    union(routes(from, to).iter().flat_map(|route| {
        route.iter().map(leg).reduce(|a, b| intersect(&a, &b)).unwrap_or_default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{span, weekdays};

    #[test]
    fn the_inverse_pair_or_both_usd_legs_rate_a_currency() {
        let january = span("2024-01-01", "2024-01-31");
        let inverse = HashMap::from([("TWDJPY=X".to_string(), weekdays(january))]);
        assert_eq!(rated(Currency::JPY, Currency::TWD, &inverse), vec![january]);

        // One USD leg alone prices nothing; both legs price their overlap.
        let mut cross = HashMap::from([("USDVND=X".to_string(), weekdays(january))]);
        assert_eq!(rated(Currency::VND, Currency::TWD, &cross), vec![]);
        cross.insert("USDTWD=X".into(), weekdays(span("2024-01-15", "2024-02-29")));
        assert_eq!(rated(Currency::VND, Currency::TWD, &cross), vec![span(
            "2024-01-15",
            "2024-01-31"
        )]);
    }
}
