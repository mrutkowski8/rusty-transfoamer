use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;

use chrono::NaiveDate;
use serde::Serialize;

use super::coverage::{Selection, SelectionOptions};
use super::structure::{Kind, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub message: String,
}

impl Finding {
    pub fn new(
        severity: Severity,
        code: &'static str,
        path: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Finding {
            severity,
            code,
            path: path.map(str::to_owned),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Good,
    Conditional,
    Rejected,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Good => "good",
            Verdict::Conditional => "conditional",
            Verdict::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejection {
    pub sieve: u8,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Ok,
    Broken,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileReport {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    pub status: FileStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteSource {
    Own,
    DayLevel,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpirationReport {
    pub expiration: NaiveDate,
    pub quotes: QuoteSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_because: Option<String>,
    pub provider_values: bool,
    pub trades: BTreeMap<Side, &'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageReport {
    pub session_bars: usize,
    pub bars_with_a_valid_quote: usize,
    pub bars_with_a_call_put_pair: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opening_placeholder: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayAudit {
    pub root: String,
    pub date: String,
    pub folder: String,
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected: Option<Rejection>,
    pub conditions: Vec<Reason>,
    pub notes: Vec<Reason>,
    pub layouts: BTreeSet<String>,
    pub expirations: Vec<ExpirationReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageReport>,
    pub findings: Vec<Finding>,
    pub files: Vec<FileReport>,
}

impl DayAudit {
    pub fn new(root: &str, date: &str, folder: String) -> Self {
        DayAudit {
            root: root.to_owned(),
            date: date.to_owned(),
            folder,
            verdict: Verdict::Good,
            rejected: None,
            conditions: Vec::new(),
            notes: Vec::new(),
            layouts: BTreeSet::new(),
            expirations: Vec::new(),
            coverage: None,
            findings: Vec::new(),
            files: Vec::new(),
        }
    }

    pub fn reject(&mut self, sieve: u8, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        self.findings
            .push(Finding::new(Severity::Error, code, None, message.clone()));
        let earlier = self.rejected.as_ref().is_some_and(|r| r.sieve <= sieve);
        if !earlier {
            self.rejected = Some(Rejection {
                sieve,
                code,
                message,
            });
        }
        self.verdict = Verdict::Rejected;
    }

    pub fn condition(&mut self, code: &'static str, message: impl Into<String>) {
        if !self.conditions.iter().any(|r| r.code == code) {
            self.conditions.push(Reason {
                code,
                message: message.into(),
            });
        }
    }

    pub fn note(&mut self, code: &'static str, message: impl Into<String>) {
        if !self.notes.iter().any(|r| r.code == code) {
            self.notes.push(Reason {
                code,
                message: message.into(),
            });
        }
    }

    pub fn finish(&mut self) {
        if self.verdict != Verdict::Rejected && !self.conditions.is_empty() {
            self.verdict = Verdict::Conditional;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFolder {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub instrument_days: usize,
    pub good: usize,
    pub conditional: usize,
    pub rejected: usize,
    pub skipped_folders: usize,
    pub days_outside_date_range: usize,
    pub files: usize,
    pub broken_files: usize,
    pub selected_dates: usize,
    pub selected_instrument_days: usize,
    pub rejections_by_code: BTreeMap<&'static str, usize>,
    pub conditions_by_code: BTreeMap<&'static str, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReferenceInfo {
    pub day: String,
    pub input: String,
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub step: u8,
    pub program_version: &'static str,
    pub input: String,
    pub calendar: &'static str,
    pub reference: ReferenceInfo,
    pub options: SelectionOptions,
    pub filters: super::Filters,
    pub summary: Summary,
    pub selection: Selection,
    pub skipped_folders: Vec<SkippedFolder>,
    pub instrument_days: Vec<DayAudit>,
}

pub fn summarize(days: &[DayAudit], skipped: usize, selection: &Selection) -> Summary {
    let mut s = Summary {
        instrument_days: days.len(),
        skipped_folders: skipped,
        selected_dates: selection.dates.len(),
        selected_instrument_days: selection.good.len() + selection.conditional.len(),
        ..Summary::default()
    };
    for day in days {
        match day.verdict {
            Verdict::Good => s.good += 1,
            Verdict::Conditional => s.conditional += 1,
            Verdict::Rejected => s.rejected += 1,
        }
        if let Some(r) = &day.rejected {
            *s.rejections_by_code.entry(r.code).or_default() += 1;
        }
        for c in &day.conditions {
            *s.conditions_by_code.entry(c.code).or_default() += 1;
        }
        s.files += day.files.len();
        s.broken_files += day
            .files
            .iter()
            .filter(|f| f.status == FileStatus::Broken)
            .count();
    }
    s
}

/// files skipped for what they are, not files left unread after an early rejection
fn skipped_files(day: &DayAudit) -> String {
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &day.files {
        if let (FileStatus::Skipped, Some(code)) = (f.status, f.code) {
            *by_code.entry(code).or_default() += 1;
        }
    }
    if by_code.is_empty() {
        return String::new();
    }
    let total: usize = by_code.values().sum();
    let files = if total == 1 { "file" } else { "files" };
    let codes: Vec<String> = if by_code.len() == 1 {
        by_code.keys().map(|c| c.to_string()).collect()
    } else {
        by_code.iter().map(|(c, n)| format!("{n} {c}")).collect()
    };
    format!(" ({total} {files} skipped: {})", codes.join(", "))
}

pub fn write_log(report: &Report, out_dir: &Path, out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "step 0 - input: {}", report.input)?;
    writeln!(
        out,
        "reference day {} from {}",
        report.reference.day, report.reference.input
    )?;
    if !report.filters.is_empty() {
        writeln!(out, "filters: {}", report.filters.describe())?;
    }
    if report.summary.days_outside_date_range > 0 {
        writeln!(
            out,
            "  {} day folders outside the date range, not audited",
            report.summary.days_outside_date_range
        )?;
    }
    for skipped in &report.skipped_folders {
        writeln!(
            out,
            "  SKIPPED {}: {} - {}",
            skipped.path, skipped.code, skipped.message
        )?;
    }
    for day in &report.instrument_days {
        let verdict = match (&day.verdict, &day.rejected) {
            (Verdict::Rejected, Some(r)) => {
                format!("REJECTED (sieve {}) {} - {}", r.sieve, r.code, r.message)
            }
            (Verdict::Conditional, _) => {
                let codes: Vec<&str> = day.conditions.iter().map(|c| c.code).collect();
                format!("conditional: {}", codes.join(", "))
            }
            _ => "good".to_owned(),
        };
        writeln!(
            out,
            "  {:<5} {}  {}{}",
            day.root,
            day.date,
            verdict,
            skipped_files(day)
        )?;
        for f in day.findings.iter().filter(|f| f.severity != Severity::Info) {
            if day
                .rejected
                .as_ref()
                .is_some_and(|r| r.code == f.code && r.message == f.message)
            {
                continue;
            }
            let label = if f.severity == Severity::Error {
                "ERROR"
            } else {
                "WARN "
            };
            match &f.path {
                Some(p) => writeln!(out, "      {label} {p}: {} - {}", f.code, f.message)?,
                None => writeln!(out, "      {label} {} - {}", f.code, f.message)?,
            }
        }
    }
    let s = &report.summary;
    writeln!(
        out,
        "summary: {} instrument-days - {} good, {} conditional, {} rejected; {} folders skipped; {} files, {} broken",
        s.instrument_days,
        s.good,
        s.conditional,
        s.rejected,
        s.skipped_folders,
        s.files,
        s.broken_files
    )?;
    let sel = &report.selection;
    if sel.dates.is_empty() {
        writeln!(
            out,
            "selection ({}): {}",
            report.options.describe(),
            sel.empty_message.as_deref().unwrap_or("no dates selected")
        )?;
    } else {
        let first = sel.dates.first().unwrap();
        let last = sel.dates.last().unwrap();
        writeln!(
            out,
            "selection ({}): {} dates from {first} to {last}, {} good and {} conditional instrument-days",
            report.options.describe(),
            sel.dates.len(),
            sel.good.len(),
            sel.conditional.len()
        )?;
        let shown: Vec<String> = sel.dates.iter().map(|d| d.to_string()).collect();
        writeln!(out, "  dates: {}", shown.join(", "))?;
    }
    writeln!(
        out,
        "outputs: {}",
        out_dir.to_string_lossy().replace('\\', "/")
    )?;
    Ok(())
}
