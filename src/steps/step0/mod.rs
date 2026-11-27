pub mod calendar;
pub mod coverage;
pub mod expirations;
pub mod qualification;
pub mod report;
pub mod rows;
pub mod structure;

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::NaiveDate;
use coverage::{SelectionOptions, select};
use expirations::DteWindow;
use report::{ReferenceInfo, Report, SkippedFolder, summarize};
use serde::Serialize;
use structure::{PathClass, References, classify};

use crate::input::parquet_footer::read_footer_bytes;
use crate::input::source::Source;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentDay {
    pub root: String,
    pub date: String,
}

impl FromStr for InstrumentDay {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (root, date) = text
            .split_once('/')
            .ok_or_else(|| format!("expected ROOT/YYYY-MM-DD, got '{text}'"))?;
        if !is_root(root) {
            return Err(format!(
                "root must be upper-case letters such as SPXW, got '{root}'"
            ));
        }
        if !is_iso_date(date) {
            return Err(format!("date must look like YYYY-MM-DD, got '{date}'"));
        }
        Ok(InstrumentDay {
            root: root.to_owned(),
            date: date.to_owned(),
        })
    }
}

impl fmt::Display for InstrumentDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.root, self.date)
    }
}

pub fn is_root(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_uppercase())
}

/// shape check only, 2023-02-30 passes
pub fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let shape_ok = bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !shape_ok {
        return false;
    }
    let month: u32 = text[5..7].parse().unwrap_or(0);
    let day: u32 = text[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// limits on what is audited; empty means everything
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Filters {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub dte: Option<DteWindow>,
}

impl Filters {
    /// a folder name that is not a real date is kept, so it gets reported
    pub fn takes_date(&self, date: &str) -> bool {
        match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
            Ok(d) => self.from.is_none_or(|f| d >= f) && self.to.is_none_or(|t| d <= t),
            Err(_) => true,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.from.is_none() && self.to.is_none() && self.dte.is_none()
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(f) = self.from {
            parts.push(format!("from {f}"));
        }
        if let Some(t) = self.to {
            parts.push(format!("to {t}"));
        }
        if let Some(w) = self.dte {
            parts.push(format!("dte {w}"));
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub input: PathBuf,
    /// empty: audit every day folder of the input
    pub days: Vec<InstrumentDay>,
    pub reference_day: String,
    pub reference_input: Option<PathBuf>,
    pub selection: SelectionOptions,
    pub filters: Filters,
    pub workers: usize,
}

#[derive(Debug)]
pub enum RunError {
    Input(String),
    Reference(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Input(m) => write!(f, "input: {m}"),
            RunError::Reference(m) => write!(f, "reference day: {m}"),
        }
    }
}

pub fn run(request: &Request) -> Result<Report, RunError> {
    let source = Source::open(&request.input).map_err(|e| RunError::Input(e.to_string()))?;
    let reference_source = match &request.reference_input {
        Some(p) => Source::open(p).map_err(|e| RunError::Reference(e.to_string()))?,
        None => source.clone(),
    };
    let refs = build_references(&reference_source, &request.reference_day)?;

    let (all_days, skipped) = if request.days.is_empty() {
        discover(&source).map_err(|e| RunError::Input(e.to_string()))?
    } else {
        (request.days.clone(), Vec::new())
    };
    let (days, outside): (Vec<InstrumentDay>, Vec<InstrumentDay>) = all_days
        .into_iter()
        .partition(|d| request.filters.takes_date(&d.date));

    let audits = audit_all(&source, &refs, &days, request.workers, request.filters.dte)?;
    let selection = select(&audits, &request.selection);
    let mut summary = summarize(&audits, skipped.len(), &selection);
    summary.days_outside_date_range = outside.len();
    Ok(Report {
        step: 0,
        program_version: env!("CARGO_PKG_VERSION"),
        input: request.input.to_string_lossy().replace('\\', "/"),
        calendar: calendar::SOURCE,
        reference: ReferenceInfo {
            day: refs.day.clone(),
            input: reference_source.path().to_string_lossy().replace('\\', "/"),
            roots: refs.roots().into_iter().collect(),
        },
        options: request.selection.clone(),
        filters: request.filters.clone(),
        summary,
        selection,
        skipped_folders: skipped,
        instrument_days: audits,
    })
}

pub fn build_references(source: &Source, day: &str) -> Result<References, RunError> {
    let mut refs = References {
        day: day.to_owned(),
        ..References::default()
    };
    let roots = source
        .top_dirs()
        .map_err(|e| RunError::Reference(e.to_string()))?;
    let mut reader = source
        .reader()
        .map_err(|e| RunError::Reference(e.to_string()))?;
    for root in roots
        .iter()
        .filter(|r| is_root(r) && source.has_dir(r, day))
    {
        let listing = source
            .list_day(root, day)
            .map_err(|e| RunError::Reference(e.to_string()))?;
        let mut files = Vec::new();
        for rel in &listing.files {
            let path = rel.to_string_lossy().replace('\\', "/");
            let PathClass::Data(id) = classify(&path) else {
                continue;
            };
            let bytes = reader
                .read(&format!("{root}/{day}/{path}"))
                .map_err(|e| RunError::Reference(format!("{root}/{day}/{path}: {e}")))?;
            let footer = read_footer_bytes(&bytes)
                .map_err(|e| RunError::Reference(format!("{root}/{day}/{path}: {e}")))?;
            files.push((id, footer));
        }
        refs.add_day(root, &files).map_err(RunError::Reference)?;
    }
    if refs.schemas.is_empty() {
        return Err(RunError::Reference(format!(
            "no root folder holds the reference day {day} in {}",
            source.path().display()
        )));
    }
    Ok(refs)
}

pub fn discover(source: &Source) -> std::io::Result<(Vec<InstrumentDay>, Vec<SkippedFolder>)> {
    let mut days = Vec::new();
    let mut skipped = Vec::new();
    for root in source.top_dirs()? {
        if !is_root(&root) {
            skipped.push(SkippedFolder {
                path: root.clone(),
                code: "not_a_root_folder",
                message: "top-level folder name is not a root such as SPXW".into(),
            });
            continue;
        }
        for name in source.sub_dirs(&root)? {
            if is_iso_date(&name) {
                days.push(InstrumentDay {
                    root: root.clone(),
                    date: name,
                });
            } else {
                skipped.push(SkippedFolder {
                    path: format!("{root}/{name}"),
                    code: "not_a_date_folder",
                    message: "folder name is not a date".into(),
                });
            }
        }
    }
    Ok((days, skipped))
}

fn audit_all(
    source: &Source,
    refs: &References,
    days: &[InstrumentDay],
    workers: usize,
    dte: Option<DteWindow>,
) -> Result<Vec<report::DayAudit>, RunError> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<report::DayAudit>>> = Mutex::new(vec![None; days.len()]);
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers.max(1).min(days.len().max(1)) {
            scope.spawn(|| {
                let mut reader = match source.reader() {
                    Ok(r) => r,
                    Err(e) => {
                        errors.lock().unwrap().push(e.to_string());
                        return;
                    }
                };
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(day) = days.get(i) else { break };
                    let audit = qualification::audit_day(
                        source,
                        &mut reader,
                        refs,
                        &day.root,
                        &day.date,
                        dte,
                    );
                    results.lock().unwrap()[i] = Some(audit);
                }
            });
        }
    });
    if let Some(e) = errors.into_inner().unwrap().into_iter().next() {
        return Err(RunError::Input(e));
    }
    Ok(results
        .into_inner()
        .unwrap()
        .into_iter()
        .flatten()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_instrument_days() {
        let day: InstrumentDay = "SPXW/2023-01-10".parse().unwrap();
        assert_eq!(
            day,
            InstrumentDay {
                root: "SPXW".into(),
                date: "2023-01-10".into()
            }
        );
        assert!("SPXW-2023-01-10".parse::<InstrumentDay>().is_err());
        assert!("spxw/2023-01-10".parse::<InstrumentDay>().is_err());
        assert!("SPXW/2023-13-10".parse::<InstrumentDay>().is_err());
        assert!("SPXW/2023-04-27 (1)".parse::<InstrumentDay>().is_err());
    }

    #[test]
    fn date_range_filter() {
        let d = |t: &str| NaiveDate::parse_from_str(t, "%Y-%m-%d").ok();
        let f = Filters {
            from: d("2023-01-09"),
            to: d("2023-01-13"),
            dte: None,
        };
        assert!(f.takes_date("2023-01-09") && f.takes_date("2023-01-13"));
        assert!(!f.takes_date("2023-01-06") && !f.takes_date("2023-01-16"));
        assert!(f.takes_date("2023-02-30"));
        assert!(Filters::default().takes_date("2026-07-30"));
        assert_eq!(f.describe(), "from 2023-01-09, to 2023-01-13");
    }

    #[test]
    fn checks_date_shape() {
        assert!(is_iso_date("2023-01-10"));
        assert!(!is_iso_date("2023-1-10"));
        assert!(!is_iso_date("2023-01-32"));
        assert!(!is_iso_date("20230110xx"));
    }
}
