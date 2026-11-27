use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, Days, NaiveDate, Weekday};
use serde::Serialize;

use super::calendar::{is_trading_day, trading_day_on_or_before};

pub fn third_friday(year: i32, month: u32) -> Option<NaiveDate> {
    NaiveDate::from_weekday_of_month_opt(year, month, Weekday::Fri, 3)
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// third friday, or the trading day before it
pub fn spx_monthly(year: i32, month: u32) -> Option<NaiveDate> {
    trading_day_on_or_before(third_friday(year, month)?)
}

/// 30 days before the third friday of the next month, each moved to the trading day before a holiday
pub fn vix_monthly(year: i32, month: u32) -> Option<NaiveDate> {
    let (y, m) = next_month(year, month);
    let friday = trading_day_on_or_before(third_friday(y, m)?)?;
    trading_day_on_or_before(friday.checked_sub_days(Days::new(30))?)
}

fn months_from(date: NaiveDate) -> impl Iterator<Item = (i32, u32)> {
    let mut ym = (date.year(), date.month());
    std::iter::from_fn(move || {
        let current = ym;
        ym = next_month(ym.0, ym.1);
        Some(current)
    })
    .take(4)
}

/// am settlement: on its expiration day the series has already stopped trading
pub fn next_spx_after(date: NaiveDate) -> Option<NaiveDate> {
    months_from(date)
        .filter_map(|(y, m)| spx_monthly(y, m))
        .find(|e| *e > date)
}

pub fn next_vix_after(date: NaiveDate) -> Option<NaiveDate> {
    months_from(date)
        .filter_map(|(y, m)| vix_monthly(y, m))
        .find(|e| *e > date)
}

/// wednesdays (the trading day before a holiday), except in the week of the monthly vix expiration
pub fn next_vixw_after(date: NaiveDate) -> Option<NaiveDate> {
    let mut wednesday = date;
    while wednesday.weekday() != Weekday::Wed {
        wednesday = wednesday.checked_add_days(Days::new(1))?;
    }
    for _ in 0..8 {
        let monday = wednesday.checked_sub_days(Days::new(2))?;
        let friday = wednesday.checked_add_days(Days::new(2))?;
        let monthly_week = months_from(monday)
            .take(2)
            .filter_map(|(y, m)| vix_monthly(y, m))
            .any(|e| e >= monday && e <= friday);
        let expiry = trading_day_on_or_before(wednesday)?;
        if !monthly_week && expiry > date {
            return Some(expiry);
        }
        wednesday = wednesday.checked_add_days(Days::new(7))?;
    }
    None
}

/// mon, wed and fri; tuesday from 2022-04-18, thursday from 2022-05-11
pub fn spxw_expires_on(date: NaiveDate) -> bool {
    match date.weekday() {
        Weekday::Mon | Weekday::Wed | Weekday::Fri => true,
        Weekday::Tue => date >= NaiveDate::from_ymd_opt(2022, 4, 18).unwrap(),
        Weekday::Thu => date >= NaiveDate::from_ymd_opt(2022, 5, 11).unwrap(),
        _ => false,
    }
}

/// days to expiration in calendar days, both ends included (--dte)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DteWindow {
    pub min: u32,
    pub max: u32,
}

impl DteWindow {
    pub fn contains(&self, date: NaiveDate, expiration: NaiveDate) -> bool {
        let days = (expiration - date).num_days();
        days >= self.min as i64 && days <= self.max as i64
    }
}

impl FromStr for DteWindow {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bad = || format!("dte must be days such as 0, or a range such as 0-45, got '{text}'");
        let number = |t: &str| t.trim().parse::<u32>().map_err(|_| bad());
        let (min, max) = match text.split_once('-') {
            Some((a, b)) => (number(a)?, number(b)?),
            None => {
                let n = number(text)?;
                (n, n)
            }
        };
        if min > max {
            return Err(bad());
        }
        Ok(DteWindow { min, max })
    }
}

impl fmt::Display for DteWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}-{}", self.min, self.max)
        }
    }
}

pub const SPXW_CONTINUOUS_DAYS: u64 = 14;
pub const SPXW_MIN_REACH_DAYS: i64 = 35;

/// the series a day must list first: the spxw 0DTE on its weekdays, else the nearest by product rules.
/// none for spxw on a weekday without a 0DTE and for other roots
pub fn nearest_expected(root: &str, date: NaiveDate) -> Result<Option<NaiveDate>, String> {
    let next = match root {
        "SPXW" => return Ok(spxw_expires_on(date).then_some(date)),
        "SPX" => next_spx_after(date),
        "VIX" => next_vix_after(date),
        "VIXW" => next_vixw_after(date),
        _ => return Ok(None),
    };
    next.map(Some)
        .ok_or_else(|| format!("the nearest {root} expiration is outside the calendar"))
}

/// spxw only: what is missing from the chain, the daily expirations of the next two weeks and the reach,
/// both limited to the --dte window. empty when the chain is complete
pub fn chain_gaps(
    root: &str,
    date: NaiveDate,
    usable: &BTreeSet<NaiveDate>,
    window: Option<DteWindow>,
) -> Vec<String> {
    let mut gaps = Vec::new();
    if root != "SPXW" {
        return gaps;
    }
    let end = date
        .checked_add_days(Days::new(SPXW_CONTINUOUS_DAYS))
        .unwrap_or(date);
    let mut missing = Vec::new();
    let mut d = date.succ_opt().unwrap_or(date);
    while d <= end {
        let wanted = window.is_none_or(|w| w.contains(date, d));
        if is_trading_day(d) && spxw_expires_on(d) && wanted && !usable.contains(&d) {
            missing.push(d.to_string());
        }
        d = d.succ_opt().unwrap_or(end.succ_opt().unwrap());
    }
    if !missing.is_empty() {
        gaps.push(format!(
            "daily expirations missing or unusable in the next two weeks: {}",
            missing.join(", ")
        ));
    }
    let reach = usable
        .iter()
        .next_back()
        .map(|last| (*last - date).num_days())
        .unwrap_or(0);
    let need = window.map_or(SPXW_MIN_REACH_DAYS, |w| {
        SPXW_MIN_REACH_DAYS.min(w.max as i64)
    });
    if reach < need {
        gaps.push(format!(
            "the farthest usable expiration is {reach} days out, at least {need} expected"
        ));
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn spx_monthly_moves_before_good_friday() {
        assert_eq!(spx_monthly(2022, 4), Some(d("2022-04-14")));
        assert_eq!(spx_monthly(2023, 1), Some(d("2023-01-20")));
        assert_eq!(next_spx_after(d("2023-01-20")), Some(d("2023-02-17")));
        assert_eq!(next_spx_after(d("2022-04-13")), Some(d("2022-04-14")));
    }

    #[test]
    fn vix_monthly_follows_the_30_day_rule() {
        assert_eq!(vix_monthly(2023, 1), Some(d("2023-01-18")));
        assert_eq!(vix_monthly(2022, 3), Some(d("2022-03-15")));
        assert_eq!(vix_monthly(2024, 6), Some(d("2024-06-18")));
        assert_eq!(next_vix_after(d("2022-09-30")), Some(d("2022-10-19")));
        assert_eq!(next_vix_after(d("2024-12-24")), Some(d("2025-01-22")));
    }

    #[test]
    fn vixw_skips_the_monthly_week_and_moves_before_holidays() {
        assert_eq!(next_vixw_after(d("2023-01-10")), Some(d("2023-01-11")));
        assert_eq!(next_vixw_after(d("2023-01-11")), Some(d("2023-01-25")));
        assert_eq!(next_vixw_after(d("2024-12-24")), Some(d("2024-12-31")));
        assert_eq!(next_vixw_after(d("2025-04-10")), Some(d("2025-04-23")));
        assert_eq!(next_vixw_after(d("2022-05-19")), Some(d("2022-05-25")));
    }

    #[test]
    fn spxw_weekdays_grow_in_2022() {
        assert!(!spxw_expires_on(d("2022-01-04")));
        assert!(spxw_expires_on(d("2022-04-19")));
        assert!(!spxw_expires_on(d("2022-04-21")));
        assert!(spxw_expires_on(d("2023-01-12")));
    }

    #[test]
    fn spxw_chain_gaps() {
        let date = d("2023-01-13");
        let mut chain: BTreeSet<NaiveDate> = [
            "2023-01-13",
            "2023-01-17",
            "2023-01-18",
            "2023-01-19",
            "2023-01-20",
            "2023-01-23",
            "2023-01-24",
            "2023-01-25",
            "2023-01-26",
            "2023-01-27",
            "2023-02-24",
        ]
        .iter()
        .map(|t| d(t))
        .collect();
        assert!(chain_gaps("SPXW", date, &chain, None).is_empty());
        chain.remove(&d("2023-01-24"));
        let gaps = chain_gaps("SPXW", date, &chain, None);
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0].contains("2023-01-24"));
        chain.insert(d("2023-01-24"));
        chain.remove(&d("2023-02-24"));
        assert!(chain_gaps("SPXW", date, &chain, None)[0].contains("days out"));
        chain.insert(d("2023-02-24"));
        chain.remove(&d("2023-01-13"));
        assert!(chain_gaps("SPXW", date, &chain, None).is_empty());
        assert!(chain_gaps("VIX", date, &BTreeSet::new(), None).is_empty());
    }

    #[test]
    fn chain_gaps_stay_inside_the_dte_window() {
        let date = d("2023-01-13");
        let only_0dte: BTreeSet<NaiveDate> = [date].into_iter().collect();
        let zero = "0".parse().unwrap();
        assert!(chain_gaps("SPXW", date, &only_0dte, Some(zero)).is_empty());
        let week = "0-7".parse().unwrap();
        let gaps = chain_gaps("SPXW", date, &only_0dte, Some(week));
        assert_eq!(gaps.len(), 2);
        assert!(gaps[0].contains("2023-01-17") && !gaps[0].contains("2023-01-23"));
        assert!(gaps[1].contains("at least 7 expected"));
    }

    #[test]
    fn dte_window_parsing() {
        assert_eq!("0".parse(), Ok(DteWindow { min: 0, max: 0 }));
        assert_eq!("0-45".parse(), Ok(DteWindow { min: 0, max: 45 }));
        assert!("45-0".parse::<DteWindow>().is_err());
        assert!("x".parse::<DteWindow>().is_err());
        assert!("-1".parse::<DteWindow>().is_err());
        let w: DteWindow = "1-7".parse().unwrap();
        assert!(!w.contains(d("2023-01-10"), d("2023-01-10")));
        assert!(w.contains(d("2023-01-10"), d("2023-01-11")));
        assert!(w.contains(d("2023-01-10"), d("2023-01-17")));
        assert!(!w.contains(d("2023-01-10"), d("2023-01-18")));
        assert_eq!(w.to_string(), "1-7");
    }

    #[test]
    fn nearest_series_per_root() {
        assert_eq!(
            nearest_expected("SPXW", d("2023-01-13")),
            Ok(Some(d("2023-01-13")))
        );
        assert_eq!(nearest_expected("SPXW", d("2022-01-04")), Ok(None));
        assert_eq!(
            nearest_expected("VIX", d("2023-04-18")),
            Ok(Some(d("2023-04-19")))
        );
        assert_eq!(
            nearest_expected("VIXW", d("2023-04-20")),
            Ok(Some(d("2023-04-26")))
        );
        assert_eq!(
            nearest_expected("SPX", d("2023-01-20")),
            Ok(Some(d("2023-02-17")))
        );
        assert!(nearest_expected("VIX", d("2027-12-20")).is_err());
    }
}
