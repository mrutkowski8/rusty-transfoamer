use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::Serialize;

use super::calendar::is_trading_day;
use super::report::{DayAudit, QuoteSource, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Common,
    All,
    Contiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectionOptions {
    pub required: Vec<String>,
    pub optional: Vec<String>,
    pub tiers: Vec<Verdict>,
    pub mode: Mode,
}

impl Default for SelectionOptions {
    fn default() -> Self {
        SelectionOptions {
            required: vec!["SPXW".into(), "VIX".into(), "VIXW".into()],
            optional: vec!["SPX".into()],
            tiers: vec![Verdict::Good],
            mode: Mode::Common,
        }
    }
}

impl SelectionOptions {
    pub fn describe(&self) -> String {
        format!(
            "{:?}, required {}, optional {}, tiers {}",
            self.mode,
            self.required.join(","),
            if self.optional.is_empty() {
                "none".into()
            } else {
                self.optional.join(",")
            },
            self.tiers
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )
        .to_lowercase()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selected {
    pub root: String,
    pub date: NaiveDate,
    pub conditions: Vec<&'static str>,
    pub notes: Vec<&'static str>,
    pub day_level_quotes: Vec<NaiveDate>,
    pub unavailable_expirations: Vec<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExcludedDate {
    pub date: NaiveDate,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub dates: Vec<NaiveDate>,
    pub good: Vec<Selected>,
    pub conditional: Vec<Selected>,
    pub excluded_dates: Vec<ExcludedDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_message: Option<String>,
}

fn passes(day: &DayAudit, tiers: &[Verdict]) -> bool {
    day.verdict != Verdict::Rejected && tiers.contains(&day.verdict)
}

fn selected(day: &DayAudit, date: NaiveDate) -> Selected {
    Selected {
        root: day.root.clone(),
        date,
        conditions: day.conditions.iter().map(|c| c.code).collect(),
        notes: day.notes.iter().map(|c| c.code).collect(),
        day_level_quotes: day
            .expirations
            .iter()
            .filter(|e| e.quotes == QuoteSource::DayLevel)
            .map(|e| e.expiration)
            .collect(),
        unavailable_expirations: day
            .expirations
            .iter()
            .filter(|e| e.quotes == QuoteSource::Unavailable)
            .map(|e| e.expiration)
            .collect(),
    }
}

pub fn select(days: &[DayAudit], options: &SelectionOptions) -> Selection {
    let mut by_date: BTreeMap<NaiveDate, BTreeMap<&str, &DayAudit>> = BTreeMap::new();
    for day in days {
        if let Ok(date) = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d") {
            by_date
                .entry(date)
                .or_default()
                .insert(day.root.as_str(), day);
        }
    }
    let ok = |date: &NaiveDate, root: &str| {
        by_date
            .get(date)
            .and_then(|m| m.get(root))
            .is_some_and(|d| passes(d, &options.tiers))
    };
    let mut selection = Selection::default();
    let push = |selection: &mut Selection, day: &DayAudit, date: NaiveDate| {
        let entry = selected(day, date);
        if day.verdict == Verdict::Good {
            selection.good.push(entry)
        } else {
            selection.conditional.push(entry)
        }
    };

    let roots: Vec<&String> = options.required.iter().chain(&options.optional).collect();
    if options.mode == Mode::All {
        let mut dates = BTreeSet::new();
        for (date, map) in &by_date {
            for root in &roots {
                if let Some(day) = map.get(root.as_str()).filter(|d| passes(d, &options.tiers)) {
                    push(&mut selection, day, *date);
                    dates.insert(*date);
                }
            }
        }
        selection.dates = dates.into_iter().collect();
    } else {
        let mut common: Vec<NaiveDate> = by_date
            .keys()
            .copied()
            .filter(|d| options.required.iter().all(|r| ok(d, r)))
            .collect();
        if options.mode == Mode::Contiguous {
            common = longest_run(&common);
        }
        for date in &common {
            for root in &roots {
                if let Some(day) = by_date[date]
                    .get(root.as_str())
                    .filter(|d| passes(d, &options.tiers))
                {
                    push(&mut selection, day, *date);
                }
            }
        }
        for (date, map) in &by_date {
            if common.contains(date) {
                continue;
            }
            let reasons: Vec<String> = options
                .required
                .iter()
                .filter(|r| !ok(date, r))
                .map(|r| match map.get(r.as_str()) {
                    None => format!("{r}: no folder"),
                    Some(d) => match (&d.verdict, &d.rejected) {
                        (Verdict::Rejected, Some(rej)) => format!("{r}: rejected, {}", rej.code),
                        (v, _) if !options.tiers.contains(v) => {
                            format!("{r}: {}, not included by --tiers", v.as_str())
                        }
                        _ => format!("{r}: not part of the longest run"),
                    },
                })
                .collect();
            let reasons = if reasons.is_empty() {
                vec!["not part of the longest run".to_owned()]
            } else {
                reasons
            };
            selection.excluded_dates.push(ExcludedDate {
                date: *date,
                reasons,
            });
        }
        selection.dates = common;
    }
    if selection.dates.is_empty() {
        selection.empty_message = Some(match options.mode {
            Mode::All => "no instrument-day passed".to_owned(),
            _ => format!("no date where {} all passed", options.required.join(", ")),
        });
    }
    selection
}

/// on a tie the newest run wins
fn longest_run(dates: &[NaiveDate]) -> Vec<NaiveDate> {
    let mut best: Vec<NaiveDate> = Vec::new();
    let mut current: Vec<NaiveDate> = Vec::new();
    for date in dates {
        let joins = current.last().is_some_and(|last| {
            let mut d = last.succ_opt().unwrap();
            while d < *date && !is_trading_day(d) {
                d = d.succ_opt().unwrap();
            }
            d == *date
        });
        if !joins {
            current.clear();
        }
        current.push(*date);
        if current.len() >= best.len() {
            best = current.clone();
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::step0::report::Reason;

    fn day(root: &str, date: &str, verdict: Verdict) -> DayAudit {
        let mut d = DayAudit::new(root, date, String::new());
        match verdict {
            Verdict::Rejected => d.reject(5, "quote_coverage_gap", "test"),
            Verdict::Conditional => {
                d.conditions.push(Reason {
                    code: "early_close",
                    message: String::new(),
                });
                d.finish();
            }
            Verdict::Good => {}
        }
        d
    }

    fn days() -> Vec<DayAudit> {
        let mut v = Vec::new();
        for date in [
            "2023-01-04",
            "2023-01-09",
            "2023-01-10",
            "2023-01-11",
            "2023-01-12",
            "2023-01-13",
            "2023-01-20",
        ] {
            for root in ["SPXW", "VIX", "VIXW", "SPX"] {
                v.push(day(root, date, Verdict::Good));
            }
        }
        v.push(day("SPXW", "2022-02-22", Verdict::Good));
        v.push(day("VIX", "2022-02-22", Verdict::Rejected));
        v.push(day("VIXW", "2022-02-22", Verdict::Good));
        v.push(day("SPXW", "2022-11-25", Verdict::Conditional));
        v.push(day("VIX", "2022-11-25", Verdict::Good));
        v.push(day("VIXW", "2022-11-25", Verdict::Good));
        v
    }

    fn dates(s: &Selection) -> Vec<String> {
        s.dates.iter().map(|d| d.to_string()).collect()
    }

    #[test]
    fn common_mode_keeps_gaps_and_adds_optional_spx() {
        let s = select(&days(), &SelectionOptions::default());
        assert_eq!(
            dates(&s),
            [
                "2023-01-04",
                "2023-01-09",
                "2023-01-10",
                "2023-01-11",
                "2023-01-12",
                "2023-01-13",
                "2023-01-20"
            ]
        );
        assert_eq!(s.good.len(), 7 * 4);
        let excluded: Vec<String> = s
            .excluded_dates
            .iter()
            .map(|e| e.date.to_string())
            .collect();
        assert_eq!(excluded, ["2022-02-22", "2022-11-25"]);
        assert!(s.excluded_dates[0].reasons[0].contains("VIX: rejected"));
    }

    #[test]
    fn conditional_days_join_only_when_asked() {
        let options = SelectionOptions {
            tiers: vec![Verdict::Good, Verdict::Conditional],
            ..SelectionOptions::default()
        };
        let s = select(&days(), &options);
        assert!(dates(&s).contains(&"2022-11-25".to_owned()));
        assert_eq!(s.conditional.len(), 1);
    }

    #[test]
    fn conditional_tier_alone_lists_only_conditional_days() {
        let options = SelectionOptions {
            tiers: vec![Verdict::Conditional],
            mode: Mode::All,
            ..SelectionOptions::default()
        };
        let s = select(&days(), &options);
        assert!(s.good.is_empty());
        assert_eq!(s.conditional.len(), 1);
        assert_eq!(dates(&s), ["2022-11-25"]);

        let common = select(
            &days(),
            &SelectionOptions {
                tiers: vec![Verdict::Conditional],
                ..SelectionOptions::default()
            },
        );
        assert!(common.dates.is_empty());
        assert!(
            common.excluded_dates[0]
                .reasons
                .iter()
                .any(|r| r == "SPXW: good, not included by --tiers")
        );
    }

    #[test]
    fn contiguous_mode_keeps_the_longest_run() {
        let options = SelectionOptions {
            mode: Mode::Contiguous,
            ..SelectionOptions::default()
        };
        let s = select(&days(), &options);
        assert_eq!(
            dates(&s),
            [
                "2023-01-09",
                "2023-01-10",
                "2023-01-11",
                "2023-01-12",
                "2023-01-13"
            ]
        );
    }

    #[test]
    fn all_mode_passes_every_good_instrument_day() {
        let options = SelectionOptions {
            mode: Mode::All,
            ..SelectionOptions::default()
        };
        let s = select(&days(), &options);
        assert!(dates(&s).contains(&"2022-02-22".to_owned()));
        assert_eq!(s.good.len(), 7 * 4 + 2 + 2);
    }

    #[test]
    fn empty_selection_says_why() {
        let s = select(
            &[day("VIX", "2023-01-10", Verdict::Good)],
            &SelectionOptions::default(),
        );
        assert!(s.dates.is_empty());
        assert_eq!(
            s.empty_message.as_deref(),
            Some("no date where SPXW, VIX, VIXW all passed")
        );
    }
}
