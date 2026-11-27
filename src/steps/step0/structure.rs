use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::Serialize;

use crate::input::parquet_footer::FooterSummary;
use crate::input::table::canonical_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Quote,
    Greeks,
    ImpliedVol,
    Ohlc,
    OpenInterest,
    DayLevelQuote,
}

impl Kind {
    pub fn schema_kind(self) -> Kind {
        match self {
            Kind::DayLevelQuote => Kind::Quote,
            k => k,
        }
    }

    pub fn has_bars(self) -> bool {
        self != Kind::OpenInterest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Bar {
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "5m")]
    M5,
}

impl Bar {
    pub fn minutes(self) -> u32 {
        match self {
            Bar::M1 => 1,
            Bar::M5 => 5,
        }
    }

    pub fn expected_for(root: &str) -> Bar {
        if root.starts_with("VIX") {
            Bar::M5
        } else {
            Bar::M1
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Call,
    Put,
}

impl Side {
    pub fn right(self) -> &'static str {
        match self {
            Side::Call => "CALL",
            Side::Put => "PUT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileId {
    pub path: String,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bar: Option<Bar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathClass {
    Data(FileId),
    NotParquet,
    Unsupported(String),
}

fn parse_bar(text: &str) -> Option<Bar> {
    match text {
        "1m" => Some(Bar::M1),
        "5m" => Some(Bar::M5),
        _ => None,
    }
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    (text.len() == 10)
        .then(|| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
        .flatten()
}

pub fn classify(path: &str) -> PathClass {
    let Some(stem_path) = path.strip_suffix(".parquet") else {
        return PathClass::NotParquet;
    };
    let parts: Vec<&str> = stem_path.split('/').collect();
    let data = |kind, bar, expiration, side| {
        PathClass::Data(FileId {
            path: path.to_owned(),
            kind,
            bar,
            expiration,
            side,
        })
    };
    match parts.as_slice() {
        ["open_interest"] => data(Kind::OpenInterest, None, None, None),
        [name] if name.starts_with("quote_") => match parse_bar(&name["quote_".len()..]) {
            Some(bar) => data(Kind::DayLevelQuote, Some(bar), None, None),
            None => {
                PathClass::Unsupported(format!("unknown file {path} at the top of the day folder"))
            }
        },
        [dir, file] => {
            let Some((kind_text, bar_text)) = dir.rsplit_once('_') else {
                return PathClass::Unsupported(format!("unknown folder {dir}"));
            };
            let (Some(bar), Some(kind)) = (parse_bar(bar_text), kind_of(kind_text)) else {
                return PathClass::Unsupported(format!("unknown folder {dir}"));
            };
            match kind {
                Kind::Quote | Kind::Greeks => match parse_date(file) {
                    Some(exp) => data(kind, Some(bar), Some(exp), None),
                    None => PathClass::Unsupported(format!(
                        "file name {file} is not an expiration date"
                    )),
                },
                _ => {
                    let side = match file.rsplit_once('_') {
                        Some((exp, "call")) => parse_date(exp).map(|e| (e, Side::Call)),
                        Some((exp, "put")) => parse_date(exp).map(|e| (e, Side::Put)),
                        _ => None,
                    };
                    match side {
                        Some((exp, side)) => data(kind, Some(bar), Some(exp), Some(side)),
                        None => PathClass::Unsupported(format!(
                            "file name {file} is not <expiration>_<call|put>"
                        )),
                    }
                }
            }
        }
        _ => PathClass::Unsupported(format!("{path} is outside the known folder layout")),
    }
}

fn kind_of(text: &str) -> Option<Kind> {
    match text {
        "quote" => Some(Kind::Quote),
        "greeks_first_order" => Some(Kind::Greeks),
        "implied_vol" => Some(Kind::ImpliedVol),
        "ohlc" => Some(Kind::Ohlc),
        _ => None,
    }
}

/// `plain`, or `c_` with the order of its first four key columns, a to d
pub fn layout(footer: &FooterSummary) -> String {
    let names: Vec<&str> = footer.columns.iter().map(|c| c.name.as_str()).collect();
    if !names.iter().any(|n| n.starts_with("c_")) {
        return "plain".to_owned();
    }
    let keys: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| n.starts_with("c_"))
        .take(4)
        .collect();
    let order = match keys.as_slice() {
        ["c_expiration", "c_right", "c_strike", "c_symbol"] => "A",
        ["c_strike", "c_symbol", "c_expiration", "c_right"] => "B",
        ["c_right", "c_strike", "c_symbol", "c_expiration"] => "C",
        ["c_symbol", "c_expiration", "c_right", "c_strike"] => "D",
        _ => "other",
    };
    format!("c_{order}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TypeClass {
    Float,
    Integer,
    Text,
    /// a time-zone timestamp or new york time text
    Time,
}

fn type_class(name: &str, data_type: &str) -> Option<TypeClass> {
    let is_time_name = name.ends_with("timestamp");
    match data_type {
        "Float64" => Some(TypeClass::Float),
        "Int64" => Some(TypeClass::Integer),
        "Utf8" if is_time_name => Some(TypeClass::Time),
        "Utf8" => Some(TypeClass::Text),
        t if t.starts_with("Timestamp(") && is_time_name => Some(TypeClass::Time),
        _ => None,
    }
}

pub type ExpectedSchema = BTreeMap<String, TypeClass>;

pub fn schema_of(footer: &FooterSummary) -> Result<ExpectedSchema, String> {
    let mut schema = ExpectedSchema::new();
    for col in &footer.columns {
        let name = canonical_name(&col.name).to_owned();
        let class = type_class(&name, &col.data_type).ok_or_else(|| {
            format!(
                "column {} has an unexpected type {}",
                col.name, col.data_type
            )
        })?;
        if schema.insert(name.clone(), class).is_some() {
            return Err(format!("column {name} appears twice"));
        }
    }
    Ok(schema)
}

pub fn schema_problems(footer: &FooterSummary, expected: &ExpectedSchema) -> Vec<String> {
    let actual = match schema_of(footer) {
        Ok(s) => s,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    for (name, class) in expected {
        match actual.get(name) {
            None => problems.push(format!("missing column {name}")),
            Some(c) if c != class => {
                let raw = footer
                    .columns
                    .iter()
                    .find(|c| canonical_name(&c.name) == name)
                    .unwrap();
                problems.push(
                    format!(
                        "column {} is {}, expected {:?}",
                        raw.name, raw.data_type, class
                    )
                    .to_lowercase(),
                );
            }
            _ => {}
        }
    }
    for name in actual.keys().filter(|n| !expected.contains_key(*n)) {
        problems.push(format!("unknown column {name}"));
    }
    problems
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct References {
    pub day: String,
    pub schemas: BTreeMap<String, BTreeMap<Kind, ExpectedSchema>>,
}

impl References {
    pub fn get(&self, root: &str, kind: Kind) -> Option<&ExpectedSchema> {
        self.schemas.get(root)?.get(&kind.schema_kind())
    }

    pub fn roots(&self) -> BTreeSet<String> {
        self.schemas.keys().cloned().collect()
    }

    /// every file of a kind must agree
    pub fn add_day(&mut self, root: &str, files: &[(FileId, FooterSummary)]) -> Result<(), String> {
        let mut by_kind: BTreeMap<Kind, ExpectedSchema> = BTreeMap::new();
        for (id, footer) in files {
            let kind = id.kind.schema_kind();
            let schema =
                schema_of(footer).map_err(|e| format!("{root} reference file {}: {e}", id.path))?;
            match by_kind.get(&kind) {
                Some(existing) if existing != &schema => {
                    return Err(format!(
                        "{root} reference files of kind {kind:?} disagree ({})",
                        id.path
                    ));
                }
                Some(_) => {}
                None => {
                    by_kind.insert(kind, schema);
                }
            }
        }
        self.schemas.insert(root.to_owned(), by_kind);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::parquet_footer::Column;

    fn footer(cols: &[(&str, &str)]) -> FooterSummary {
        FooterSummary {
            rows: 1,
            row_groups: 1,
            created_by: None,
            columns: cols
                .iter()
                .map(|(n, t)| Column {
                    name: (*n).into(),
                    data_type: (*t).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn classifies_known_paths() {
        let id = |p: &str| match classify(p) {
            PathClass::Data(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(id("quote_1m/2023-01-10.parquet").kind, Kind::Quote);
        let iv = id("implied_vol_5m/2023-01-18_put.parquet");
        assert_eq!(
            (iv.kind, iv.bar, iv.side),
            (Kind::ImpliedVol, Some(Bar::M5), Some(Side::Put))
        );
        assert_eq!(id("open_interest.parquet").kind, Kind::OpenInterest);
        assert_eq!(id("quote_1m.parquet").kind, Kind::DayLevelQuote);
        assert_eq!(classify("desktop.ini"), PathClass::NotParquet);
        assert_eq!(
            classify("quote_1m/2023-05-17.parquet.part"),
            PathClass::NotParquet
        );
        assert!(matches!(
            classify("snapshots/20260903T133007Z/quote.parquet"),
            PathClass::Unsupported(_)
        ));
        assert!(matches!(
            classify("latest/quote.parquet"),
            PathClass::Unsupported(_)
        ));
    }

    #[test]
    fn layout_names_the_column_order() {
        assert_eq!(
            layout(&footer(&[("symbol", "Utf8"), ("strike", "Float64")])),
            "plain"
        );
        assert_eq!(
            layout(&footer(&[
                ("c_strike", "Float64"),
                ("c_symbol", "Utf8"),
                ("c_expiration", "Utf8"),
                ("c_right", "Utf8")
            ])),
            "c_B"
        );
    }

    #[test]
    fn schema_check_ignores_order_and_prefix_but_not_names_or_types() {
        let reference = schema_of(&footer(&[
            ("c_strike", "Float64"),
            ("c_right", "Utf8"),
            ("timestamp", "Utf8"),
        ]))
        .unwrap();
        let plain = footer(&[
            ("timestamp", "Timestamp(µs, \"America/New_York\")"),
            ("right", "Utf8"),
            ("strike", "Float64"),
        ]);
        assert!(schema_problems(&plain, &reference).is_empty());

        let renamed = footer(&[
            ("strike", "Float64"),
            ("side", "Utf8"),
            ("timestamp", "Utf8"),
        ]);
        assert_eq!(
            schema_problems(&renamed, &reference),
            ["missing column right", "unknown column side"]
        );

        let text_strike = footer(&[("strike", "Utf8"), ("right", "Utf8"), ("timestamp", "Utf8")]);
        assert_eq!(
            schema_problems(&text_strike, &reference),
            ["column strike is utf8, expected float"]
        );
    }
}
