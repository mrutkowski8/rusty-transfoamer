use chrono::{Datelike, Days, NaiveDate, NaiveTime, Weekday};

pub const SOURCE: &str = "NYSE holiday and early-close calendar 2021-2027, checked against exchange_calendars 4.13.3 (XNYS)";

const FIRST_YEAR: i32 = 2021;
const LAST_YEAR: i32 = 2027;

const CLOSED: &[(&str, &str)] = &[
    ("2021-01-01", "New Year's Day"),
    ("2021-01-18", "Martin Luther King Jr. Day"),
    ("2021-02-15", "Washington's Birthday"),
    ("2021-04-02", "Good Friday"),
    ("2021-05-31", "Memorial Day"),
    ("2021-07-05", "Independence Day (observed)"),
    ("2021-09-06", "Labor Day"),
    ("2021-11-25", "Thanksgiving Day"),
    ("2021-12-24", "Christmas Day (observed)"),
    ("2022-01-17", "Martin Luther King Jr. Day"),
    ("2022-02-21", "Washington's Birthday"),
    ("2022-04-15", "Good Friday"),
    ("2022-05-30", "Memorial Day"),
    ("2022-06-20", "Juneteenth (observed)"),
    ("2022-07-04", "Independence Day"),
    ("2022-09-05", "Labor Day"),
    ("2022-11-24", "Thanksgiving Day"),
    ("2022-12-26", "Christmas Day (observed)"),
    ("2023-01-02", "New Year's Day (observed)"),
    ("2023-01-16", "Martin Luther King Jr. Day"),
    ("2023-02-20", "Washington's Birthday"),
    ("2023-04-07", "Good Friday"),
    ("2023-05-29", "Memorial Day"),
    ("2023-06-19", "Juneteenth"),
    ("2023-07-04", "Independence Day"),
    ("2023-09-04", "Labor Day"),
    ("2023-11-23", "Thanksgiving Day"),
    ("2023-12-25", "Christmas Day"),
    ("2024-01-01", "New Year's Day"),
    ("2024-01-15", "Martin Luther King Jr. Day"),
    ("2024-02-19", "Washington's Birthday"),
    ("2024-03-29", "Good Friday"),
    ("2024-05-27", "Memorial Day"),
    ("2024-06-19", "Juneteenth"),
    ("2024-07-04", "Independence Day"),
    ("2024-09-02", "Labor Day"),
    ("2024-11-28", "Thanksgiving Day"),
    ("2024-12-25", "Christmas Day"),
    ("2025-01-01", "New Year's Day"),
    (
        "2025-01-09",
        "National Day of Mourning for President Carter (unscheduled)",
    ),
    ("2025-01-20", "Martin Luther King Jr. Day"),
    ("2025-02-17", "Washington's Birthday"),
    ("2025-04-18", "Good Friday"),
    ("2025-05-26", "Memorial Day"),
    ("2025-06-19", "Juneteenth"),
    ("2025-07-04", "Independence Day"),
    ("2025-09-01", "Labor Day"),
    ("2025-11-27", "Thanksgiving Day"),
    ("2025-12-25", "Christmas Day"),
    ("2026-01-01", "New Year's Day"),
    ("2026-01-19", "Martin Luther King Jr. Day"),
    ("2026-02-16", "Washington's Birthday"),
    ("2026-04-03", "Good Friday"),
    ("2026-05-25", "Memorial Day"),
    ("2026-06-19", "Juneteenth"),
    ("2026-07-03", "Independence Day (observed)"),
    ("2026-09-07", "Labor Day"),
    ("2026-11-26", "Thanksgiving Day"),
    ("2026-12-25", "Christmas Day"),
    ("2027-01-01", "New Year's Day"),
    ("2027-01-18", "Martin Luther King Jr. Day"),
    ("2027-02-15", "Washington's Birthday"),
    ("2027-03-26", "Good Friday"),
    ("2027-05-31", "Memorial Day"),
    ("2027-06-18", "Juneteenth (observed)"),
    ("2027-07-05", "Independence Day (observed)"),
    ("2027-09-06", "Labor Day"),
    ("2027-11-25", "Thanksgiving Day"),
    ("2027-12-24", "Christmas Day (observed)"),
];

/// stocks close at 13:00 on these days, index options at 13:15
const EARLY_CLOSES: &[&str] = &[
    "2021-11-26",
    "2022-11-25",
    "2023-07-03",
    "2023-11-24",
    "2024-07-03",
    "2024-11-29",
    "2024-12-24",
    "2025-07-03",
    "2025-11-28",
    "2025-12-24",
    "2026-11-27",
    "2026-12-24",
    "2027-11-26",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Open { early_close: bool },
    Closed(&'static str),
    NotCovered,
}

pub fn session_open() -> NaiveTime {
    NaiveTime::from_hms_opt(9, 30, 0).unwrap()
}

/// the last bar of the export is 16:00 every day, half days included
pub fn export_end() -> NaiveTime {
    NaiveTime::from_hms_opt(16, 0, 0).unwrap()
}

pub fn day(date: NaiveDate) -> Day {
    if !(FIRST_YEAR..=LAST_YEAR).contains(&date.year()) {
        return Day::NotCovered;
    }
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return Day::Closed("weekend");
    }
    let key = date.format("%Y-%m-%d").to_string();
    if let Some((_, name)) = CLOSED.iter().find(|(d, _)| *d == key) {
        return Day::Closed(name);
    }
    Day::Open {
        early_close: EARLY_CLOSES.contains(&key.as_str()),
    }
}

pub fn is_trading_day(date: NaiveDate) -> bool {
    matches!(day(date), Day::Open { .. })
}

pub fn session_close(date: NaiveDate) -> Option<NaiveTime> {
    match day(date) {
        Day::Open { early_close: false } => Some(export_end()),
        Day::Open { early_close: true } => NaiveTime::from_hms_opt(13, 15, 0),
        _ => None,
    }
}

pub fn trading_day_on_or_before(date: NaiveDate) -> Option<NaiveDate> {
    let mut d = date;
    for _ in 0..10 {
        match day(d) {
            Day::Open { .. } => return Some(d),
            Day::NotCovered => return None,
            Day::Closed(_) => d = d.checked_sub_days(Days::new(1))?,
        }
    }
    None
}

pub fn previous_trading_day(date: NaiveDate) -> Option<NaiveDate> {
    trading_day_on_or_before(date.checked_sub_days(Days::new(1))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn holidays_weekends_and_unscheduled_closures_are_closed() {
        assert_eq!(
            day(d("2023-01-16")),
            Day::Closed("Martin Luther King Jr. Day")
        );
        assert_eq!(
            day(d("2025-01-09")),
            Day::Closed("National Day of Mourning for President Carter (unscheduled)")
        );
        assert_eq!(
            day(d("2026-07-03")),
            Day::Closed("Independence Day (observed)")
        );
        assert_eq!(day(d("2023-04-29")), Day::Closed("weekend"));
        assert_eq!(day(d("2023-01-10")), Day::Open { early_close: false });
        assert_eq!(day(d("2020-12-31")), Day::NotCovered);
    }

    #[test]
    fn half_days_close_at_13_15() {
        assert_eq!(
            session_close(d("2022-11-25")),
            NaiveTime::from_hms_opt(13, 15, 0)
        );
        assert_eq!(
            session_close(d("2024-12-24")),
            NaiveTime::from_hms_opt(13, 15, 0)
        );
        assert_eq!(
            session_close(d("2023-01-10")),
            NaiveTime::from_hms_opt(16, 0, 0)
        );
        assert_eq!(session_close(d("2023-01-16")), None);
    }

    #[test]
    fn previous_trading_day_skips_weekends_and_holidays() {
        assert_eq!(previous_trading_day(d("2023-01-17")), Some(d("2023-01-13")));
        assert_eq!(previous_trading_day(d("2024-10-15")), Some(d("2024-10-14")));
        assert_eq!(
            trading_day_on_or_before(d("2022-04-15")),
            Some(d("2022-04-14"))
        );
    }

    #[test]
    fn tables_are_sorted_and_inside_the_covered_years() {
        let closed: Vec<&str> = CLOSED.iter().map(|(d, _)| *d).collect();
        assert!(closed.windows(2).all(|w| w[0] < w[1]));
        assert!(EARLY_CLOSES.windows(2).all(|w| w[0] < w[1]));
        for text in closed.iter().chain(EARLY_CLOSES) {
            let date = d(text);
            assert!((FIRST_YEAR..=LAST_YEAR).contains(&date.year()));
            assert!(
                !matches!(date.weekday(), Weekday::Sat | Weekday::Sun),
                "{text}"
            );
        }
    }
}
