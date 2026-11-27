use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Timelike};
use chrono_tz::America::New_York;

use super::expirations::DteWindow;
use super::structure::{Bar, FileId, Kind, Side};
use crate::input::table::{NULL_I64, Table, Times, new_york_times};

pub const OPEN_MINUTE: u32 = 9 * 60 + 30;
pub const EXPORT_END_MINUTE: u32 = 16 * 60;

#[derive(Debug, Clone)]
pub struct Context {
    pub root: String,
    pub date: NaiveDate,
    pub close_minute: u32,
    /// minutes to take off every time when the clock runs in utc but says new york
    pub shift_back: i64,
    pub keep_quote_values: bool,
    pub dte: Option<DteWindow>,
}

impl Context {
    /// without --dte every expiration is in the window
    pub fn in_window(&self, expiration: NaiveDate) -> bool {
        self.dte.is_none_or(|w| w.contains(self.date, expiration))
    }
}

/// expiration, strike × 1000, side, minute of the day
pub type BarKey = (NaiveDate, i64, Side, u32);
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuoteValues {
    pub bid: f64,
    pub ask: f64,
    pub counts: [i64; 6],
}

impl QuoteValues {
    /// prices can differ in the last bits between two exports of the same quote
    pub fn same_as(&self, other: &QuoteValues) -> bool {
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 || (a.is_nan() && b.is_nan());
        close(self.bid, other.bid) && close(self.ask, other.ask) && self.counts == other.counts
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileRows {
    pub rows: usize,
    pub broken: Option<(&'static str, String)>,
    pub conditions: Vec<(&'static str, String)>,
    pub grid_gap: Option<String>,
    pub outside: Option<Outside>,
    pub converted_interval: bool,
    pub after_close_rows: usize,
    pub invalid_rows: usize,
    /// rows of expirations outside --dte, not checked further
    pub outside_window_rows: usize,
    pub expirations: BTreeSet<NaiveDate>,
    pub contracts: BTreeSet<(NaiveDate, i64, Side)>,
    pub valid_minutes: BTreeMap<NaiveDate, BTreeSet<u32>>,
    /// minutes where at least one strike has a valid call and a valid put
    pub pair_minutes: BTreeMap<NaiveDate, BTreeSet<u32>>,
    /// per expiration: rows at 09:30 and how many are bid = ask = 0 placeholders
    pub opening: BTreeMap<NaiveDate, (usize, usize)>,
    pub underlying: BTreeMap<u32, f64>,
    pub provider_iv: HashMap<(NaiveDate, i64, Side, u32), f64>,
    pub quote_values: HashMap<BarKey, QuoteValues>,
    pub stamp_dates: BTreeSet<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outside {
    pub rows: usize,
    pub example: String,
    /// every time lands on the session grid after moving it back by the new york utc offset
    pub fits_after_shift: bool,
}

/// minutes new york is behind utc on that date: 300 in winter, 240 in summer
pub fn new_york_offset_minutes(date: NaiveDate) -> i64 {
    let noon = date.and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    New_York
        .from_local_datetime(&noon)
        .single()
        .map(|t| -(t.offset().fix().local_minus_utc() as i64) / 60)
        .unwrap_or(300)
}

fn minute_of_day(t: &NaiveDateTime) -> Option<u32> {
    (t.second() == 0 && t.nanosecond() == 0).then(|| t.hour() * 60 + t.minute())
}

fn strike_key(strike: f64) -> Option<i64> {
    (strike.is_finite() && strike > 0.0).then(|| (strike * 1000.0).round() as i64)
}

fn side_of(text: Option<&str>) -> Option<Side> {
    match text {
        Some("CALL") | Some("C") => Some(Side::Call),
        Some("PUT") | Some("P") => Some(Side::Put),
        _ => None,
    }
}

fn negative_or_infinite(v: f64) -> bool {
    v < 0.0 || v.is_infinite()
}

pub fn check_rows(table: &Table, id: &FileId, ctx: &Context) -> FileRows {
    let mut out = FileRows {
        rows: table.rows,
        ..FileRows::default()
    };
    let n = table.rows;

    // times
    let Some(time_col) = table.col("timestamp") else {
        out.broken = Some(("parquet_wrong_schema", "no timestamp column".into()));
        return out;
    };
    let mut times = match new_york_times(time_col) {
        Times::Parsed(t) => t,
        Times::Reformatted { times, format } => {
            out.conditions.push((
                "timestamp_reformatted",
                format!("text timestamps read with the format {format}"),
            ));
            times
        }
        Times::Unparseable { example } => {
            out.broken = Some((
                "timestamp_unparseable",
                format!("cannot read the time '{example}'"),
            ));
            return out;
        }
        Times::Missing => {
            out.broken = Some((
                "parquet_wrong_schema",
                "timestamp column has no usable type".into(),
            ));
            return out;
        }
    };
    if ctx.shift_back != 0 {
        for t in times.iter_mut() {
            *t -= Duration::minutes(ctx.shift_back);
        }
    }

    // dates
    for t in &times {
        out.stamp_dates.insert(t.date());
    }
    if id.kind == Kind::OpenInterest && out.stamp_dates.iter().all(|d| *d < ctx.date) {
        let shown: Vec<String> = out.stamp_dates.iter().map(|d| d.to_string()).collect();
        out.broken = Some((
            "stale_open_interest",
            format!("stamped {} instead of {}", shown.join(", "), ctx.date),
        ));
        return out;
    }
    if out.stamp_dates.iter().any(|d| *d != ctx.date) {
        let other: Vec<String> = out
            .stamp_dates
            .iter()
            .filter(|d| **d != ctx.date)
            .map(|d| d.to_string())
            .collect();
        let rows = times.iter().filter(|t| t.date() != ctx.date).count();
        out.broken = Some((
            "content_mismatch",
            format!(
                "{rows} rows are dated {} instead of {}",
                other.join(", "),
                ctx.date
            ),
        ));
        return out;
    }

    // identity
    let symbols = table.str("symbol");
    if let Some(sym) = symbols {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for row in 0..n {
            *counts.entry(sym.get(row).unwrap_or("null")).or_default() += 1;
        }
        if counts.keys().any(|s| *s != ctx.root) {
            let shown: Vec<String> = counts.iter().map(|(s, c)| format!("{s} × {c}")).collect();
            out.broken = Some((
                "content_mismatch",
                format!(
                    "symbol column holds {} in a {} folder",
                    shown.join(", "),
                    ctx.root
                ),
            ));
            return out;
        }
    }
    let exp_col = table.str("expiration");
    let mut exps: Vec<Option<NaiveDate>> = vec![id.expiration; n];
    if let Some(col) = exp_col {
        let parsed: Vec<Option<NaiveDate>> = col
            .dict
            .iter()
            .map(|t| NaiveDate::parse_from_str(t, "%Y-%m-%d").ok())
            .collect();
        for (row, e) in exps.iter_mut().enumerate() {
            *e = match col.codes[row] {
                crate::input::table::NULL_CODE => None,
                c => parsed[c as usize],
            };
        }
        if let Some(expected) = id.expiration {
            let wrong: BTreeSet<String> = exps
                .iter()
                .filter(|e| **e != Some(expected))
                .map(|e| {
                    e.map(|d| d.to_string())
                        .unwrap_or_else(|| "unreadable".into())
                })
                .collect();
            if !wrong.is_empty() {
                let shown: Vec<String> = wrong.into_iter().collect();
                out.broken = Some((
                    "content_mismatch",
                    format!("file name says {expected}, rows say {}", shown.join(", ")),
                ));
                return out;
            }
        }
    }
    let sides: Vec<Option<Side>> = match table.str("right") {
        Some(col) => (0..n).map(|row| side_of(col.get(row))).collect(),
        None => vec![id.side; n],
    };
    if let Some(expected) = id.side {
        let wrong = sides.iter().filter(|s| **s != Some(expected)).count();
        if wrong > 0 {
            out.broken = Some((
                "content_mismatch",
                format!(
                    "{wrong} rows are not {} in a {} file",
                    expected.right(),
                    expected.right()
                ),
            ));
            return out;
        }
    }
    let strikes: Vec<Option<i64>> = match table.f64("strike") {
        Some(v) => v.iter().map(|s| strike_key(*s)).collect(),
        None => vec![None; n],
    };

    // multi-expiration files (day-level quotes, open interest) can hold expirations outside --dte
    let skip: Vec<bool> = exps
        .iter()
        .map(|e| e.is_some_and(|e| !ctx.in_window(e)))
        .collect();
    out.outside_window_rows = skip.iter().filter(|s| **s).count();

    // invalid values
    let f = |name: &str| table.f64(name);
    let i = |name: &str| table.i64(name);
    let price_cols: Vec<&[f64]> = [
        "bid",
        "ask",
        "open",
        "high",
        "low",
        "close",
        "vwap",
        "underlying_price",
    ]
    .iter()
    .filter_map(|c| f(c))
    .collect();
    let count_cols: Vec<&[i64]> = ["bid_size", "ask_size", "volume", "count", "open_interest"]
        .iter()
        .filter_map(|c| i(c))
        .collect();
    let mut invalid = vec![false; n];
    for row in 0..n {
        let bad_identity = strikes[row].is_none() || sides[row].is_none() || exps[row].is_none();
        let bad_price = price_cols.iter().any(|c| negative_or_infinite(c[row]));
        let bad_count = count_cols.iter().any(|c| c[row] != NULL_I64 && c[row] < 0);
        invalid[row] = !skip[row] && (bad_identity || bad_price || bad_count);
    }
    out.invalid_rows = invalid.iter().filter(|b| **b).count();
    if out.invalid_rows > 0 {
        out.conditions.push((
            "invalid_values",
            format!("{} rows with impossible values excluded", out.invalid_rows),
        ));
    }

    // duplicates
    let key_of = |row: usize| -> Option<(NaiveDate, i64, Side, u32)> {
        let minute = if id.kind.has_bars() {
            minute_of_day(&times[row])?
        } else {
            0
        };
        Some((exps[row]?, strikes[row]?, sides[row]?, minute))
    };
    let mut first_row: HashMap<(NaiveDate, i64, Side, u32), usize> = HashMap::with_capacity(n);
    let (mut exact, mut conflicting) = (0usize, 0usize);
    let mut duplicate = vec![false; n];
    for row in 0..n {
        if invalid[row] || skip[row] {
            continue;
        }
        let Some(key) = key_of(row) else { continue };
        match first_row.get(&key) {
            None => {
                first_row.insert(key, row);
            }
            Some(&first) => {
                duplicate[row] = true;
                if table.rows_equal(first, row) {
                    exact += 1
                } else {
                    conflicting += 1
                }
            }
        }
    }
    if conflicting > 0 {
        out.broken = Some((
            "conflicting_duplicates",
            format!("{conflicting} rows repeat a key with different values"),
        ));
        return out;
    }
    if exact > 0 {
        out.conditions.push((
            "exact_duplicates",
            format!("{exact} identical repeated rows to remove"),
        ));
    }

    if id.kind == Kind::OpenInterest {
        for row in (0..n).filter(|r| !invalid[*r] && !skip[*r]) {
            if let (Some(e), Some(s), Some(side)) = (exps[row], strikes[row], sides[row]) {
                out.contracts.insert((e, s, side));
                out.expirations.insert(e);
            }
        }
        return out;
    }

    // session window and bar interval
    let declared = id.bar.unwrap_or(Bar::expected_for(&ctx.root));
    let step = declared.minutes();
    let mut outside_rows = 0;
    let mut off_grid = 0;
    let mut example = String::new();
    let mut minutes = vec![0u32; n];
    for row in 0..n {
        match minute_of_day(&times[row]) {
            Some(m) if (OPEN_MINUTE..=EXPORT_END_MINUTE).contains(&m) => {
                if !(m - OPEN_MINUTE).is_multiple_of(step) {
                    off_grid += 1;
                }
                minutes[row] = m;
            }
            _ => {
                outside_rows += 1;
                if example.is_empty() {
                    example = times[row].format("%H:%M:%S").to_string();
                }
            }
        }
    }
    if outside_rows > 0 {
        let offset = new_york_offset_minutes(ctx.date);
        let fits = times.iter().all(|t| {
            minute_of_day(&(*t - Duration::minutes(offset))).is_some_and(|m| {
                (OPEN_MINUTE..=EXPORT_END_MINUTE).contains(&m)
                    && (m - OPEN_MINUTE).is_multiple_of(step)
            })
        });
        out.outside = Some(Outside {
            rows: outside_rows,
            example: example.clone(),
            fits_after_shift: fits,
        });
        out.broken = Some((
            "outside_session",
            format!("{outside_rows} rows outside 09:30-16:00, e.g. {example}"),
        ));
        return out;
    }
    if off_grid > 0 {
        out.broken = Some((
            "wrong_bar_interval",
            format!("{off_grid} rows are off the {step}-minute grid"),
        ));
        return out;
    }
    let expected_bar = Bar::expected_for(&ctx.root);
    if declared != expected_bar {
        if declared == Bar::M1 && expected_bar == Bar::M5 {
            out.converted_interval = true;
        } else {
            out.broken = Some((
                "wrong_bar_interval",
                format!(
                    "{}-minute bars where {}-minute bars are expected",
                    step,
                    expected_bar.minutes()
                ),
            ));
            return out;
        }
    }
    out.after_close_rows = (0..n)
        .filter(|r| !skip[*r] && minutes[*r] > ctx.close_minute)
        .count();

    // contracts and the time grid
    let required_step = expected_bar.minutes();
    let required: Vec<u32> = (OPEN_MINUTE..=ctx.close_minute)
        .step_by(required_step as usize)
        .collect();
    let mut per_minute: HashMap<(NaiveDate, u32), usize> = HashMap::new();
    let mut unknown_identity: HashMap<u32, usize> = HashMap::new();
    for row in 0..n {
        if duplicate[row] || skip[row] {
            continue;
        }
        match (exps[row], strikes[row], sides[row]) {
            (Some(e), Some(s), Some(side)) => {
                out.contracts.insert((e, s, side));
                out.expirations.insert(e);
                *per_minute.entry((e, minutes[row])).or_default() += 1;
            }
            _ => *unknown_identity.entry(minutes[row]).or_default() += 1,
        }
    }
    let grid_kind = matches!(
        id.kind,
        Kind::Quote | Kind::Greeks | Kind::ImpliedVol | Kind::DayLevelQuote
    );
    if grid_kind && out.grid_gap.is_none() {
        let mut contracts_per_exp: BTreeMap<NaiveDate, usize> = BTreeMap::new();
        for (e, _, _) in &out.contracts {
            *contracts_per_exp.entry(*e).or_default() += 1;
        }
        'outer: for (exp, contracts) in &contracts_per_exp {
            for m in &required {
                let have = per_minute.get(&(*exp, *m)).copied().unwrap_or(0)
                    + unknown_identity.get(m).copied().unwrap_or(0);
                if have < *contracts {
                    let short = required
                        .iter()
                        .filter(|m| {
                            per_minute.get(&(*exp, **m)).copied().unwrap_or(0)
                                + unknown_identity.get(m).copied().unwrap_or(0)
                                < *contracts
                        })
                        .count();
                    out.grid_gap = Some(format!(
                        "expiration {exp}: {short} of {} session bars incomplete, first at {:02}:{:02}",
                        required.len(),
                        m / 60,
                        m % 60
                    ));
                    break 'outer;
                }
            }
        }
    }

    // quotes: valid two-sided minutes, call-put pairs, opening placeholders, values for comparison
    if matches!(id.kind, Kind::Quote | Kind::DayLevelQuote) {
        let (bid, ask) = (f("bid"), f("ask"));
        if let (Some(bid), Some(ask)) = (bid, ask) {
            let mut sides_seen: HashMap<(NaiveDate, u32, i64), u8> = HashMap::new();
            for row in 0..n {
                if invalid[row] || duplicate[row] || skip[row] {
                    continue;
                }
                let Some(e) = exps[row] else { continue };
                if minutes[row] == OPEN_MINUTE {
                    let counts = out.opening.entry(e).or_default();
                    counts.0 += 1;
                    let empty = |v: f64| v == 0.0 || v.is_nan();
                    if empty(bid[row]) && empty(ask[row]) {
                        counts.1 += 1;
                    }
                }
                if bid[row] > 0.0 && ask[row] > bid[row] {
                    out.valid_minutes.entry(e).or_default().insert(minutes[row]);
                    if let (Some(s), Some(side)) = (strikes[row], sides[row]) {
                        let seen = sides_seen.entry((e, minutes[row], s)).or_default();
                        *seen |= if side == Side::Call { 1 } else { 2 };
                        if *seen == 3 {
                            out.pair_minutes.entry(e).or_default().insert(minutes[row]);
                        }
                    }
                }
            }
            if ctx.keep_quote_values {
                let ints: Vec<Option<&[i64]>> = [
                    "bid_size",
                    "ask_size",
                    "bid_exchange",
                    "ask_exchange",
                    "bid_condition",
                    "ask_condition",
                ]
                .iter()
                .map(|c| i(c))
                .collect();
                for row in (0..n).filter(|r| !skip[*r]) {
                    let Some(key) = key_of(row) else { continue };
                    let mut counts = [0i64; 6];
                    for (slot, col) in counts.iter_mut().zip(&ints) {
                        *slot = col.map(|c| c[row]).unwrap_or(NULL_I64);
                    }
                    out.quote_values.insert(
                        key,
                        QuoteValues {
                            bid: bid[row],
                            ask: ask[row],
                            counts,
                        },
                    );
                }
            }
        }
    }

    // provider values: underlying and IV
    if matches!(id.kind, Kind::Greeks | Kind::ImpliedVol) {
        if let Some(under) = f("underlying_price") {
            for row in 0..n {
                if under[row].is_finite() && under[row] > 0.0 {
                    out.underlying.entry(minutes[row]).or_insert(under[row]);
                }
            }
        }
        if let Some(iv) = f("implied_vol") {
            for row in 0..n {
                if invalid[row]
                    || duplicate[row]
                    || skip[row]
                    || !(iv[row].is_finite() && iv[row] > 0.0)
                {
                    continue;
                }
                if let (Some(e), Some(s), Some(side)) = (exps[row], strikes[row], sides[row]) {
                    out.provider_iv.insert((e, s, side, minutes[row]), iv[row]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use bytes::Bytes;
    use parquet::arrow::ArrowWriter;

    use crate::input::table::read_table;

    pub struct Row<'a> {
        pub strike: f64,
        pub right: &'a str,
        pub time: &'a str,
        pub bid: f64,
        pub ask: f64,
    }

    pub fn quote_table(symbol: &str, exp: &str, rows: &[Row]) -> Table {
        let fields = vec![
            Field::new("c_symbol", DataType::Utf8, true),
            Field::new("c_expiration", DataType::Utf8, true),
            Field::new("c_strike", DataType::Float64, true),
            Field::new("c_right", DataType::Utf8, true),
            Field::new("timestamp", DataType::Utf8, true),
            Field::new("bid", DataType::Float64, true),
            Field::new("ask", DataType::Float64, true),
            Field::new("bid_size", DataType::Int64, true),
        ];
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(vec![symbol; rows.len()])),
            Arc::new(StringArray::from(vec![exp; rows.len()])),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.strike).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.right).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.time).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.bid).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.ask).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(vec![1i64; rows.len()])),
        ];
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
        let mut buf = Vec::new();
        let mut w = ArrowWriter::try_new(&mut buf, schema, None).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        read_table(Bytes::from(buf)).unwrap()
    }

    fn ctx(root: &str, close: u32) -> Context {
        Context {
            root: root.into(),
            date: NaiveDate::from_ymd_opt(2023, 1, 10).unwrap(),
            close_minute: close,
            shift_back: 0,
            keep_quote_values: false,
            dte: None,
        }
    }

    fn quote_id(exp: &str, bar: Bar) -> FileId {
        FileId {
            path: format!("quote/{exp}.parquet"),
            kind: Kind::Quote,
            bar: Some(bar),
            expiration: NaiveDate::parse_from_str(exp, "%Y-%m-%d").ok(),
            side: None,
        }
    }

    fn full_day(last: u32) -> Vec<(String, f64, &'static str)> {
        let mut v = Vec::new();
        let mut m = OPEN_MINUTE;
        while m <= last {
            let t = format!("2023-01-10T{:02}:{:02}:00.000", m / 60, m % 60);
            v.push((t.clone(), 20.0, "CALL"));
            v.push((t, 20.0, "PUT"));
            m += 5;
        }
        v
    }

    fn rows_of<'a>(day: &'a [(String, f64, &'static str)]) -> Vec<Row<'a>> {
        day.iter()
            .map(|(t, s, r)| Row {
                strike: *s,
                right: r,
                time: t,
                bid: 1.0,
                ask: 1.2,
            })
            .collect()
    }

    #[test]
    fn clean_file_has_no_findings_and_full_coverage() {
        let day = full_day(EXPORT_END_MINUTE);
        let table = quote_table("VIX", "2023-01-18", &rows_of(&day));
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken, None);
        assert!(out.conditions.is_empty());
        assert_eq!(out.grid_gap, None);
        let exp = NaiveDate::from_ymd_opt(2023, 1, 18).unwrap();
        assert_eq!(out.valid_minutes[&exp].len(), 79);
        assert_eq!(out.pair_minutes[&exp].len(), 79);
    }

    #[test]
    fn rows_outside_the_dte_window_are_not_checked() {
        let day = full_day(EXPORT_END_MINUTE);
        let table = quote_table("VIX", "2023-01-18", &rows_of(&day));
        let id = FileId {
            path: "quote_5m.parquet".into(),
            kind: Kind::DayLevelQuote,
            bar: Some(Bar::M5),
            expiration: None,
            side: None,
        };
        let mut c = ctx("VIX", EXPORT_END_MINUTE);
        c.dte = Some("0-5".parse().unwrap());
        let out = check_rows(&table, &id, &c);
        assert_eq!(out.outside_window_rows, day.len());
        assert!(out.valid_minutes.is_empty() && out.expirations.is_empty());
        assert_eq!(out.grid_gap, None);
        c.dte = Some("0-10".parse().unwrap());
        let out = check_rows(&table, &id, &c);
        assert_eq!(out.outside_window_rows, 0);
        assert_eq!(out.valid_minutes.values().next().unwrap().len(), 79);
    }

    #[test]
    fn lone_call_is_a_valid_quote_but_not_a_pair() {
        let day = full_day(EXPORT_END_MINUTE);
        let mut rows = rows_of(&day);
        for r in rows.iter_mut() {
            if r.right == "PUT" && r.time.contains("T10:00") {
                r.bid = 0.0;
            }
        }
        let table = quote_table("VIX", "2023-01-18", &rows);
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        let exp = NaiveDate::from_ymd_opt(2023, 1, 18).unwrap();
        assert!(out.valid_minutes[&exp].contains(&600));
        assert!(!out.pair_minutes[&exp].contains(&600));
        assert_eq!(out.pair_minutes[&exp].len(), 78);
    }

    #[test]
    fn missing_hour_is_a_grid_gap() {
        let day: Vec<_> = full_day(EXPORT_END_MINUTE)
            .into_iter()
            .filter(|(t, _, _)| !t.contains("T11:"))
            .collect();
        let table = quote_table("VIX", "2023-01-18", &rows_of(&day));
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert!(out.grid_gap.unwrap().contains("12 of 79"));
    }

    #[test]
    fn wrong_symbol_and_wrong_expiration_break_the_file() {
        let day = full_day(EXPORT_END_MINUTE);
        let table = quote_table("VIXW", "2023-01-18", &rows_of(&day));
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken.unwrap().0, "content_mismatch");
        let table = quote_table("VIX", "2023-02-15", &rows_of(&day));
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert!(out.broken.unwrap().1.contains("file name says 2023-01-18"));
    }

    #[test]
    fn duplicates_exact_and_conflicting() {
        let day = full_day(EXPORT_END_MINUTE);
        let mut rows = rows_of(&day);
        rows.push(Row {
            strike: 20.0,
            right: "CALL",
            time: "2023-01-10T09:30:00.000",
            bid: 1.0,
            ask: 1.2,
        });
        let table = quote_table("VIX", "2023-01-18", &rows);
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken, None);
        assert_eq!(out.conditions[0].0, "exact_duplicates");

        rows.push(Row {
            strike: 20.0,
            right: "PUT",
            time: "2023-01-10T09:30:00.000",
            bid: 9.0,
            ask: 9.5,
        });
        let table = quote_table("VIX", "2023-01-18", &rows);
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken.unwrap().0, "conflicting_duplicates");
    }

    #[test]
    fn negative_bid_and_missing_strike_are_invalid_rows_not_gaps() {
        let day = full_day(EXPORT_END_MINUTE);
        let mut rows = rows_of(&day);
        rows[3].bid = -1.0;
        rows[5].strike = f64::NAN;
        let table = quote_table("VIX", "2023-01-18", &rows);
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken, None);
        assert_eq!(out.invalid_rows, 2);
        assert_eq!(out.grid_gap, None);
    }

    #[test]
    fn utc_clock_is_outside_the_session_but_fits_after_the_shift() {
        let day: Vec<_> = full_day(EXPORT_END_MINUTE)
            .into_iter()
            .map(|(t, s, r)| {
                let h: u32 = t[11..13].parse().unwrap();
                (format!("{}{:02}{}", &t[..11], h + 5, &t[13..]), s, r)
            })
            .collect();
        let table = quote_table("VIX", "2023-01-18", &rows_of(&day));
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M5),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken.as_ref().unwrap().0, "outside_session");
        assert!(out.outside.unwrap().fits_after_shift);

        let mut shifted = ctx("VIX", EXPORT_END_MINUTE);
        shifted.shift_back = new_york_offset_minutes(shifted.date);
        let out = check_rows(&table, &quote_id("2023-01-18", Bar::M5), &shifted);
        assert_eq!(out.broken, None);
    }

    #[test]
    fn one_minute_vix_is_converted_and_checked_on_five_minute_marks() {
        let mut day = Vec::new();
        for m in OPEN_MINUTE..=EXPORT_END_MINUTE {
            day.push((
                format!("2023-01-10T{:02}:{:02}:00.000", m / 60, m % 60),
                20.0,
                "CALL",
            ));
        }
        let table = quote_table("VIX", "2023-01-18", &rows_of(&day));
        let out = check_rows(
            &table,
            &quote_id("2023-01-18", Bar::M1),
            &ctx("VIX", EXPORT_END_MINUTE),
        );
        assert_eq!(out.broken, None);
        assert!(out.converted_interval);
    }

    #[test]
    fn new_york_offset_follows_daylight_saving() {
        assert_eq!(
            new_york_offset_minutes(NaiveDate::from_ymd_opt(2023, 1, 10).unwrap()),
            300
        );
        assert_eq!(
            new_york_offset_minutes(NaiveDate::from_ymd_opt(2023, 4, 24).unwrap()),
            240
        );
    }
}
