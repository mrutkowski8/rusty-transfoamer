use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, Timelike};

use super::calendar::{self, Day};
use super::expirations::{DteWindow, chain_gaps, nearest_expected};
use super::report::{
    CoverageReport, DayAudit, ExpirationReport, FileReport, FileStatus, Finding, QuoteSource,
    Severity,
};
use super::rows::{Context, FileRows, OPEN_MINUTE, check_rows, new_york_offset_minutes};
use super::structure::{
    FileId, Kind, PathClass, References, Side, classify, layout, schema_problems,
};
use crate::input::parquet_footer::read_footer_bytes;
use crate::input::source::{DayReader, Source};
use crate::input::table::{Times, new_york_times, read_table};

#[derive(Debug, Clone)]
struct Checked {
    id: FileId,
    rows: Option<FileRows>,
    broken: Option<(&'static str, String)>,
}

impl Checked {
    fn ok(&self) -> Option<&FileRows> {
        if self.broken.is_none() {
            self.rows.as_ref()
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ExpirationCross {
    quote_conflicts: usize,
    iv_mismatches: usize,
    strike_mismatches: usize,
}

fn sister(root: &str) -> Option<&'static str> {
    match root {
        "VIX" => Some("VIXW"),
        "VIXW" => Some("VIX"),
        "SPX" => Some("SPXW"),
        "SPXW" => Some("SPX"),
        _ => None,
    }
}

pub fn audit_day(
    source: &Source,
    reader: &mut DayReader,
    refs: &References,
    root: &str,
    date_text: &str,
    dte: Option<DteWindow>,
) -> DayAudit {
    let mut audit = DayAudit::new(root, date_text, source.day_path(root, date_text));
    let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
        audit.reject(1, "not_a_date_folder", "folder name is not a date");
        return audit;
    };

    // sieve 1: folder
    if !source.has_dir(root, date_text) {
        audit.reject(1, "folder_not_found", format!("no folder {}", audit.folder));
        return audit;
    }
    let listing = match source.list_day(root, date_text) {
        Ok(l) => l,
        Err(e) => {
            audit.reject(1, "folder_unreadable", e.to_string());
            return audit;
        }
    };
    let mut data_files = Vec::new();
    let mut any_parquet = false;
    for rel in &listing.files {
        let path = rel.to_string_lossy().replace('\\', "/");
        let skip =
            |audit: &mut DayAudit, code: &'static str, severity: Severity, message: String| {
                audit
                    .findings
                    .push(Finding::new(severity, code, Some(&path), message));
                audit.files.push(FileReport {
                    path: path.clone(),
                    kind: None,
                    status: FileStatus::Skipped,
                    code: Some(code),
                    layout: None,
                    rows: None,
                    size_bytes: reader.size(&format!("{root}/{date_text}/{path}")),
                });
            };
        match classify(&path) {
            PathClass::NotParquet => skip(
                &mut audit,
                "invalid_file_type",
                Severity::Info,
                "not a .parquet file; skipped".into(),
            ),
            PathClass::Unsupported(why) => {
                any_parquet = true;
                skip(
                    &mut audit,
                    "unsupported_layout",
                    Severity::Error,
                    format!("{why}; skipped"),
                )
            }
            PathClass::Data(id) if id.kind == Kind::OpenInterest && root.starts_with("VIX") => {
                any_parquet = true;
                skip(
                    &mut audit,
                    "unexpected_open_interest",
                    Severity::Info,
                    format!("{root} never has open interest; skipped"),
                );
                audit.note(
                    "unexpected_open_interest",
                    "an open interest file was skipped",
                );
            }
            PathClass::Data(id)
                if id
                    .expiration
                    .is_some_and(|e| dte.is_some_and(|w| !w.contains(date, e))) =>
            {
                any_parquet = true;
                skip(
                    &mut audit,
                    "outside_dte_window",
                    Severity::Info,
                    format!("expiration outside --dte {}; skipped", dte.unwrap()),
                );
            }
            PathClass::Data(id) => {
                any_parquet = true;
                data_files.push(id);
            }
        }
    }
    for dir in &listing.empty_dirs {
        let dir = dir.to_string_lossy().replace('\\', "/");
        audit.findings.push(Finding::new(
            Severity::Info,
            "empty_directory",
            Some(&dir),
            "directory is empty",
        ));
    }
    if !any_parquet {
        audit.reject(1, "no_data_files", "the folder holds no .parquet files");
        return audit;
    }

    // sieve 2: calendar
    let early_close = match calendar::day(date) {
        Day::Closed(why) => {
            audit.reject(2, "market_closed", format!("no regular session: {why}"));
            skip_unread(&mut audit, &data_files, reader, root, date_text);
            return audit;
        }
        Day::NotCovered => {
            audit.reject(
                2,
                "calendar_not_covered",
                "the built-in calendar does not cover this year",
            );
            skip_unread(&mut audit, &data_files, reader, root, date_text);
            return audit;
        }
        Day::Open { early_close } => early_close,
    };
    if !refs.schemas.contains_key(root) {
        audit.reject(
            3,
            "reference_missing",
            format!("the reference day {} has no {root} folder", refs.day),
        );
        skip_unread(&mut audit, &data_files, reader, root, date_text);
        return audit;
    }
    let close = calendar::session_close(date).unwrap();
    let close_minute = close.hour() * 60 + close.minute();
    let mut ctx = Context {
        root: root.to_owned(),
        date,
        close_minute,
        shift_back: 0,
        keep_quote_values: data_files.iter().any(|f| f.kind == Kind::DayLevelQuote),
        dte,
    };

    // sieves 3 and 4, with the clock repair as a second pass
    let (files_before, findings_before) = (audit.files.len(), audit.findings.len());
    let (mut checked, mut crosses) = check_files(&mut audit, &data_files, reader, refs, &ctx);
    let bar_files: Vec<&Checked> = checked
        .iter()
        .filter(|c| c.id.kind.has_bars() && c.rows.is_some())
        .collect();
    let shift_candidate = !bar_files.is_empty()
        && bar_files.iter().all(|c| {
            c.rows
                .as_ref()
                .unwrap()
                .outside
                .as_ref()
                .is_some_and(|o| o.fits_after_shift)
        });
    if shift_candidate {
        ctx.shift_back = new_york_offset_minutes(date);
        let mut probe = DayAudit::new(root, date_text, String::new());
        let (second, second_crosses) = check_files(&mut probe, &data_files, reader, refs, &ctx);
        let ours = underlying_of(&second);
        let verdict = match sister(root) {
            None => Err("this root has no sister root".to_owned()),
            Some(s) => match underlying_series(source, s, date_text) {
                Some(theirs) => compare_underlying(&ours, &theirs),
                None => Err(format!(
                    "{s} {date_text} is not available to verify the shift"
                )),
            },
        };
        match verdict {
            Ok(evidence) => {
                audit.files.truncate(files_before);
                audit.findings.truncate(findings_before);
                audit.findings.extend(probe.findings);
                audit.files.extend(probe.files);
                checked = second;
                crosses = second_crosses;
                audit.condition(
                    "clock_shift_repaired",
                    format!("every time is {} minutes late; shifted back and verified: {evidence}", ctx.shift_back),
                );
            }
            Err(why) => audit.findings.push(Finding::new(
                Severity::Error,
                "clock_shift_unverified",
                None,
                format!("times look like UTC labelled as New York, but the shift cannot be verified: {why}"),
            )),
        }
    }

    assemble(&mut audit, &ctx, early_close, &checked, &crosses);
    audit.finish();
    audit
}

fn skip_unread(audit: &mut DayAudit, files: &[FileId], reader: &DayReader, root: &str, date: &str) {
    for id in files {
        audit.files.push(FileReport {
            path: id.path.clone(),
            kind: Some(id.kind),
            status: FileStatus::Skipped,
            code: None,
            layout: None,
            rows: None,
            size_bytes: reader.size(&format!("{root}/{date}/{}", id.path)),
        });
    }
}

/// one expiration at a time, so large days stay small in memory
fn check_files(
    audit: &mut DayAudit,
    files: &[FileId],
    reader: &mut DayReader,
    refs: &References,
    ctx: &Context,
) -> (Vec<Checked>, BTreeMap<NaiveDate, ExpirationCross>) {
    let root = ctx.root.as_str();
    let date = ctx.date.format("%Y-%m-%d").to_string();
    let mut checked: Vec<Checked> = Vec::with_capacity(files.len());
    let mut crosses = BTreeMap::new();

    let mut groups: BTreeMap<Option<NaiveDate>, Vec<&FileId>> = BTreeMap::new();
    for id in files {
        groups.entry(id.expiration).or_default().push(id);
    }
    // files without an expiration (open interest, the day-level quote file) come first
    let mut day_level_index: Option<usize> = None;
    for (exp, ids) in groups {
        let start = checked.len();
        for id in ids {
            let c = check_file(audit, id, reader, refs, ctx, root, &date);
            checked.push(c);
        }
        let Some(exp) = exp else {
            day_level_index =
                (start..checked.len()).find(|i| checked[*i].id.kind == Kind::DayLevelQuote);
            continue;
        };
        let (head, group) = checked.split_at_mut(start);
        let day_level = day_level_index.and_then(|i| head[i].ok());
        let mut cross = ExpirationCross::default();
        let quote = group
            .iter()
            .find(|c| c.id.kind == Kind::Quote)
            .and_then(|c| c.ok());
        let greeks = group
            .iter()
            .find(|c| c.id.kind == Kind::Greeks)
            .and_then(|c| c.ok());
        if let (Some(q), Some(dl)) = (quote, day_level) {
            cross.quote_conflicts = q
                .quote_values
                .iter()
                .filter(|(k, v)| dl.quote_values.get(*k).is_some_and(|w| !w.same_as(v)))
                .count();
        }
        if let (Some(q), Some(g)) = (quote, greeks) {
            let qs: BTreeSet<(i64, Side)> =
                q.contracts.iter().map(|(_, s, side)| (*s, *side)).collect();
            let gs: BTreeSet<(i64, Side)> =
                g.contracts.iter().map(|(_, s, side)| (*s, *side)).collect();
            cross.strike_mismatches = qs.symmetric_difference(&gs).count();
        }
        if let Some(g) = greeks {
            for iv in group
                .iter()
                .filter(|c| c.id.kind == Kind::ImpliedVol)
                .filter_map(|c| c.ok())
            {
                cross.iv_mismatches += iv
                    .provider_iv
                    .iter()
                    .filter(|(k, v)| {
                        g.provider_iv
                            .get(*k)
                            .is_some_and(|w| (*w - **v).abs() > 1e-9)
                    })
                    .count();
            }
        }
        for c in group.iter_mut() {
            if let Some(r) = c.rows.as_mut() {
                r.provider_iv = Default::default();
                r.quote_values = Default::default();
                r.contracts = Default::default();
            }
        }
        crosses.insert(exp, cross);
    }
    for c in checked
        .iter_mut()
        .filter(|c| c.id.kind == Kind::DayLevelQuote)
    {
        if let Some(r) = c.rows.as_mut() {
            r.quote_values = Default::default();
        }
    }
    (checked, crosses)
}

fn check_file(
    audit: &mut DayAudit,
    id: &FileId,
    reader: &mut DayReader,
    refs: &References,
    ctx: &Context,
    root: &str,
    date: &str,
) -> Checked {
    let full = format!("{root}/{date}/{}", id.path);
    let mut report = FileReport {
        path: id.path.clone(),
        kind: Some(id.kind),
        status: FileStatus::Ok,
        code: None,
        layout: None,
        rows: None,
        size_bytes: reader.size(&full),
    };
    let done = |audit: &mut DayAudit,
                report: FileReport,
                broken: Option<(&'static str, String)>,
                rows: Option<FileRows>| {
        let mut report = report;
        if let Some((code, msg)) = &broken {
            report.status = FileStatus::Broken;
            report.code = Some(*code);
            audit.findings.push(Finding::new(
                Severity::Error,
                code,
                Some(&id.path),
                msg.clone(),
            ));
        }
        audit.files.push(report);
        Checked {
            id: id.clone(),
            rows,
            broken,
        }
    };
    let bytes = match reader.read(&full) {
        Ok(b) => b,
        Err(e) => {
            return done(
                audit,
                report,
                Some(("parquet_unreadable", format!("cannot read the file: {e}"))),
                None,
            );
        }
    };
    let footer = match read_footer_bytes(&bytes) {
        Ok(f) => f,
        Err(e) => {
            return done(
                audit,
                report,
                Some(("parquet_unreadable", e.to_string())),
                None,
            );
        }
    };
    report.layout = Some(layout(&footer));
    report.rows = Some(footer.rows as usize);
    if footer.rows == 0 {
        return done(
            audit,
            report,
            Some(("parquet_empty", "the table has 0 rows".into())),
            None,
        );
    }
    let Some(expected) = refs.get(root, id.kind) else {
        return done(
            audit,
            report,
            Some((
                "parquet_wrong_schema",
                "no reference schema for this file kind".into(),
            )),
            None,
        );
    };
    let problems = schema_problems(&footer, expected);
    if !problems.is_empty() {
        return done(
            audit,
            report,
            Some(("parquet_wrong_schema", problems.join("; "))),
            None,
        );
    }
    let table = match read_table(bytes) {
        Ok(t) => t,
        Err(e) => {
            return done(
                audit,
                report,
                Some(("parquet_unreadable", format!("cannot decode the rows: {e}"))),
                None,
            );
        }
    };
    let rows = check_rows(&table, id, ctx);
    let broken = rows.broken.clone();
    done(audit, report, broken, Some(rows))
}

fn underlying_of(checked: &[Checked]) -> BTreeMap<u32, f64> {
    let mut map = BTreeMap::new();
    for c in checked.iter().filter_map(|c| c.ok()) {
        for (m, v) in &c.underlying {
            map.entry(*m).or_insert(*v);
        }
    }
    map
}

fn underlying_series(source: &Source, root: &str, date: &str) -> Option<BTreeMap<u32, f64>> {
    let listing = source.list_day(root, date).ok()?;
    let mut reader = source.reader().ok()?;
    let mut map = BTreeMap::new();
    for rel in listing
        .files
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
    {
        let PathClass::Data(id) = classify(&rel) else {
            continue;
        };
        if !matches!(id.kind, Kind::Greeks | Kind::ImpliedVol) {
            continue;
        }
        let Ok(bytes) = reader.read(&format!("{root}/{date}/{rel}")) else {
            continue;
        };
        let Ok(table) = read_table(bytes) else {
            continue;
        };
        let (Some(col), Some(under)) = (table.col("timestamp"), table.f64("underlying_price"))
        else {
            continue;
        };
        let Times::Parsed(times) = new_york_times(col) else {
            continue;
        };
        for (t, v) in times.iter().zip(under) {
            if v.is_finite() && *v > 0.0 {
                map.entry(t.hour() * 60 + t.minute()).or_insert(*v);
            }
        }
        if map.len() > 60 {
            break;
        }
    }
    (!map.is_empty()).then_some(map)
}

fn compare_underlying(
    ours: &BTreeMap<u32, f64>,
    theirs: &BTreeMap<u32, f64>,
) -> Result<String, String> {
    let shared: Vec<(&u32, &f64)> = ours
        .iter()
        .filter(|(m, _)| theirs.contains_key(m))
        .collect();
    let equal = shared
        .iter()
        .filter(|(m, v)| (*v - theirs[m]).abs() <= 1e-6 * v.abs().max(1.0))
        .count();
    if ours.is_empty() || shared.len() * 2 < ours.len() || equal != shared.len() {
        return Err(format!(
            "{equal} of {} shared minutes match the sister root's underlying",
            shared.len()
        ));
    }
    Ok(format!(
        "{equal} of {equal} underlying prices match the sister root"
    ))
}

/// minutes as 09:31-11:35, joining minutes up to 5 apart
fn span(minutes: &[u32]) -> String {
    let hm = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
    let mut parts = Vec::new();
    let mut start = minutes[0];
    let mut prev = minutes[0];
    for &m in &minutes[1..] {
        if m - prev > 5 {
            parts.push((start, prev));
            start = m;
        }
        prev = m;
    }
    parts.push((start, prev));
    parts
        .iter()
        .map(|&(a, b)| {
            if a == b {
                hm(a)
            } else {
                format!("{}-{}", hm(a), hm(b))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn assemble(
    audit: &mut DayAudit,
    ctx: &Context,
    early_close: bool,
    checked: &[Checked],
    crosses: &BTreeMap<NaiveDate, ExpirationCross>,
) {
    let (root, date, close_minute, dte) = (ctx.root.as_str(), ctx.date, ctx.close_minute, ctx.dte);
    for c in checked {
        if let Some(l) = audit
            .files
            .iter()
            .find(|f| f.path == c.id.path)
            .and_then(|f| f.layout.clone())
        {
            audit.layouts.insert(l);
        }
    }
    if early_close {
        let after: usize = checked
            .iter()
            .filter_map(|c| c.ok())
            .map(|r| r.after_close_rows)
            .sum();
        audit.condition(
            "early_close",
            format!(
                "half day, the session ends at 13:15; {after} rows after the close are outside it"
            ),
        );
    }

    let find = |kind: Kind, exp: Option<NaiveDate>, side: Option<Side>| {
        checked
            .iter()
            .find(|c| c.id.kind == kind && c.id.expiration == exp && c.id.side == side)
    };
    let day_level = checked.iter().find(|c| c.id.kind == Kind::DayLevelQuote);
    let day_level_ok = day_level.and_then(|c| c.ok());

    let mut delivered: BTreeSet<NaiveDate> =
        checked.iter().filter_map(|c| c.id.expiration).collect();
    if let Some(dl) = day_level_ok {
        delivered.extend(dl.expirations.iter().copied());
    }

    let mut valid_minutes: BTreeSet<u32> = BTreeSet::new();
    let mut pair_minutes: BTreeMap<NaiveDate, BTreeSet<u32>> = BTreeMap::new();
    let (mut opening_rows, mut opening_placeholders) = (0usize, 0usize);
    let mut available = 0usize;
    let mut gap: Option<String> = None;
    let mut used: Vec<&Checked> = Vec::new();

    for exp in &delivered {
        let own = find(Kind::Quote, Some(*exp), None);
        let cross = crosses.get(exp).cloned().unwrap_or_default();
        let dl_has = day_level_ok.is_some_and(|d| d.expirations.contains(exp));
        let mut report = ExpirationReport {
            expiration: *exp,
            quotes: QuoteSource::Unavailable,
            unavailable_because: None,
            provider_values: false,
            trades: BTreeMap::new(),
        };
        let own_ok = own.and_then(|c| c.ok());
        let quote_rows: Option<&FileRows> = match (own_ok, dl_has) {
            (Some(_), true) if cross.quote_conflicts > 0 => {
                report.unavailable_because = Some(format!(
                    "quote_sources_conflict: {} rows differ from the day-level file",
                    cross.quote_conflicts
                ));
                audit.reject(
                    5,
                    "quote_sources_conflict",
                    format!(
                        "expiration {exp}: {} rows differ between its own quote file and quote_1m.parquet",
                        cross.quote_conflicts
                    ),
                );
                None
            }
            (Some(q), _) => {
                report.quotes = QuoteSource::Own;
                used.push(own.unwrap());
                Some(q)
            }
            (None, true) => {
                report.quotes = QuoteSource::DayLevel;
                used.push(day_level.unwrap());
                audit.condition(
                    "day_level_quotes",
                    "quotes of at least one expiration come from the day-level file",
                );
                day_level_ok
            }
            (None, false) => {
                report.unavailable_because = Some(match own {
                    Some(c) => format!(
                        "{}: quote file is broken",
                        c.broken.as_ref().map(|b| b.0).unwrap_or("broken")
                    ),
                    None => "no quote file".to_owned(),
                });
                None
            }
        };
        if let Some(q) = quote_rows {
            available += 1;
            if let Some(minutes) = q.valid_minutes.get(exp) {
                valid_minutes.extend(minutes);
            }
            pair_minutes.insert(*exp, q.pair_minutes.get(exp).cloned().unwrap_or_default());
            if let Some((rows, placeholders)) = q.opening.get(exp) {
                opening_rows += rows;
                opening_placeholders += placeholders;
            }
            let gap_here = q
                .grid_gap
                .as_ref()
                .filter(|g| report.quotes == QuoteSource::Own || g.contains(&exp.to_string()));
            if gap.is_none() {
                gap = gap_here.cloned();
            }
            let greeks = find(Kind::Greeks, Some(*exp), None);
            let ivs = [
                find(Kind::ImpliedVol, Some(*exp), Some(Side::Call)),
                find(Kind::ImpliedVol, Some(*exp), Some(Side::Put)),
            ];
            report.provider_values = greeks.and_then(|c| c.ok()).is_some()
                && ivs.iter().all(|c| c.and_then(|c| c.ok()).is_some());
            if !report.provider_values {
                audit.condition(
                    "missing_vendor_values",
                    "provider IV or greeks are missing for at least one expiration",
                );
            }
            for c in std::iter::once(greeks).chain(ivs).flatten() {
                if let Some(r) = c.ok() {
                    used.push(c);
                    if gap.is_none() {
                        gap = r.grid_gap.clone().map(|g| format!("{}: {g}", c.id.path));
                    }
                }
            }
            if cross.iv_mismatches > 0 || cross.strike_mismatches > 0 {
                audit.condition(
                    "vendor_values_inconsistent",
                    "provider IV differs between the greeks and IV files, or the files list different strikes",
                );
                audit.findings.push(Finding::new(
                    Severity::Warning,
                    "vendor_values_inconsistent",
                    None,
                    format!(
                        "expiration {exp}: {} IV values differ, {} contracts in only one of quote/greeks",
                        cross.iv_mismatches, cross.strike_mismatches
                    ),
                ));
            }
            for side in [Side::Call, Side::Put] {
                let status = match find(Kind::Ohlc, Some(*exp), Some(side)) {
                    None => {
                        audit.note(
                            "ohlc_side_missing",
                            "no trades on one side of an expiration",
                        );
                        "no_file_no_trades"
                    }
                    Some(c) if c.ok().is_some() => {
                        used.push(c);
                        "ok"
                    }
                    Some(_) => {
                        audit.condition("trades_unavailable", "a trades file is broken");
                        "broken"
                    }
                };
                report.trades.insert(side, status);
            }
        } else {
            audit.condition(
                "expiration_unavailable",
                "at least one expiration has no usable quotes",
            );
        }
        audit.expirations.push(report);
    }

    let mut seen = BTreeSet::new();
    used.retain(|c| seen.insert(c.id.path.clone()));
    for c in &used {
        if let Some(r) = c.ok() {
            for (code, msg) in &r.conditions {
                audit.condition(code, msg.clone());
                audit.findings.push(Finding::new(
                    Severity::Warning,
                    code,
                    Some(&c.id.path),
                    msg.clone(),
                ));
            }
            if r.converted_interval {
                audit.note(
                    "interval_converted",
                    "1-minute bars, converted to 5-minute bars in step 1",
                );
            }
        }
    }

    // sieve 5c: open interest
    if root == "SPX" || root == "SPXW" {
        match checked.iter().find(|c| c.id.kind == Kind::OpenInterest) {
            None => audit.reject(5, "open_interest_unusable", "no open_interest.parquet"),
            Some(c) => match (&c.broken, c.rows.as_ref()) {
                (Some((code, msg)), _) => {
                    audit.reject(5, "open_interest_unusable", format!("{code}: {msg}"))
                }
                (None, Some(r)) => {
                    for (code, msg) in &r.conditions {
                        audit.condition(code, msg.clone());
                    }
                }
                _ => {}
            },
        }
    }

    if available == 0 {
        let outside = audit
            .files
            .iter()
            .any(|f| f.code == Some("outside_dte_window"))
            || checked
                .iter()
                .filter(|c| c.id.kind == Kind::DayLevelQuote)
                .filter_map(|c| c.rows.as_ref())
                .any(|r| r.outside_window_rows > 0);
        match dte {
            Some(w) if delivered.is_empty() && outside => audit.reject(
                5,
                "no_expiration_in_window",
                format!("no expiration is {w} days out (--dte)"),
            ),
            _ => audit.reject(
                5,
                "no_usable_expirations",
                "no expiration has usable quotes",
            ),
        }
        return;
    }
    if let Some(g) = gap {
        audit.reject(5, "grid_gap", g);
    }

    // sieve 5e: a call-put pair in every session bar, so a forward can be built
    let step = super::structure::Bar::expected_for(root).minutes();
    let required: Vec<u32> = (OPEN_MINUTE..=close_minute)
        .step_by(step as usize)
        .collect();
    let opening_placeholder = opening_rows > 0 && opening_placeholders == opening_rows;
    // a series expiring today stops being quoted at the close
    let only_expiring_today = pair_minutes.keys().all(|e| *e == date);
    let exempt = |m: u32| {
        (m == OPEN_MINUTE && opening_placeholder) || (m == close_minute && only_expiring_today)
    };
    let any_pair: BTreeSet<u32> = pair_minutes.values().flatten().copied().collect();
    let missing: Vec<u32> = required
        .iter()
        .copied()
        .filter(|m| !any_pair.contains(m) && !exempt(*m))
        .collect();
    if opening_placeholder {
        audit.note(
            "opening_placeholder",
            "every quote at 09:30 is the bid = ask = 0 placeholder",
        );
    }
    audit.coverage = Some(CoverageReport {
        session_bars: required.len(),
        bars_with_a_valid_quote: required
            .iter()
            .filter(|m| valid_minutes.contains(m))
            .count(),
        bars_with_a_call_put_pair: required.iter().filter(|m| any_pair.contains(m)).count(),
        opening_placeholder: opening_placeholder.then_some(true),
    });
    if !missing.is_empty() {
        audit.reject(
            5,
            "quote_coverage_gap",
            format!(
                "{} session bars without a call-put pair, {}",
                missing.len(),
                span(&missing)
            ),
        );
    }

    // the nearest expiration needs pairs too, except at the opening bar and, for a 0DTE, the closing bar
    if let Some((nearest, pairs)) = pair_minutes.range(date..).next() {
        let thin: Vec<u32> = required
            .iter()
            .copied()
            .filter(|m| !pairs.contains(m) && *m != OPEN_MINUTE)
            .filter(|m| !(*nearest == date && *m == close_minute))
            .collect();
        if !thin.is_empty() {
            audit.condition(
                "nearest_without_pairs",
                format!(
                    "the nearest expiration {nearest} has no call-put pair in {} session bars, {}",
                    thin.len(),
                    span(&thin)
                ),
            );
        }
    }

    // sieve 5f: a broken expiration counts as missing
    let usable: BTreeSet<NaiveDate> = audit
        .expirations
        .iter()
        .filter(|e| e.quotes != QuoteSource::Unavailable)
        .map(|e| e.expiration)
        .collect();
    match nearest_expected(root, date) {
        Ok(Some(e)) if !usable.contains(&e) && dte.is_none_or(|w| w.contains(date, e)) => {
            let why = if delivered.contains(&e) {
                "has no usable quotes"
            } else {
                "is missing"
            };
            audit.condition(
                "nearest_expiration_unavailable",
                format!("the nearest expiration {e} {why}"),
            );
        }
        Err(why) => audit.reject(2, "calendar_not_covered", why),
        _ => {}
    }
    // a missing expiration never rejects the day: the user of the data decides what it needs
    let gaps = chain_gaps(root, date, &usable, dte);
    if !gaps.is_empty() {
        audit.condition("expirations_missing", gaps.join("; "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underlying_comparison_needs_most_minutes_to_match() {
        let ours: BTreeMap<u32, f64> = (0..10).map(|m| (m, 20.0 + m as f64)).collect();
        let mut theirs = ours.clone();
        assert!(compare_underlying(&ours, &theirs).is_ok());
        theirs.insert(3, 99.0);
        assert!(compare_underlying(&ours, &theirs).is_err());
        let few: BTreeMap<u32, f64> = (0..3).map(|m| (m, 20.0 + m as f64)).collect();
        assert!(compare_underlying(&ours, &few).is_err());
    }

    #[test]
    fn sister_roots_pair_up() {
        assert_eq!(sister("VIX"), Some("VIXW"));
        assert_eq!(sister("SPXW"), Some("SPX"));
        assert_eq!(sister("NDX"), None);
    }
}
