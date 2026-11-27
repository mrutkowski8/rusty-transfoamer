use std::collections::HashMap;

use arrow_array::cast::AsArray;
use arrow_array::types::{
    Float32Type, Float64Type, Int32Type, Int64Type, TimestampMicrosecondType,
    TimestampMillisecondType, TimestampNanosecondType, TimestampSecondType,
};
use arrow_array::{Array, RecordBatch};
use arrow_schema::{DataType, TimeUnit};
use bytes::Bytes;
use chrono::{DateTime, NaiveDateTime};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::errors::ParquetError;

pub const NULL_I64: i64 = i64::MIN;

#[derive(Debug, Clone)]
pub struct Table {
    pub rows: usize,
    pub columns: Vec<Col>,
}

#[derive(Debug, Clone)]
pub struct Col {
    pub name: String,
    pub raw_name: String,
    pub data_type: String,
    pub data: ColData,
}

#[derive(Debug, Clone)]
pub enum ColData {
    F64(Vec<f64>),
    I64(Vec<i64>),
    Str(DictStr),
    Ts {
        values: Vec<i64>,
        unit: TimeUnit,
        tz: Option<String>,
    },
    Other,
}

/// each distinct text is stored once, the export repeats a few hundred values millions of times
#[derive(Debug, Clone, Default)]
pub struct DictStr {
    pub dict: Vec<String>,
    pub codes: Vec<u32>,
    lookup: HashMap<String, u32>,
}

pub const NULL_CODE: u32 = u32::MAX;

impl DictStr {
    fn push(&mut self, value: Option<&str>) {
        let code = match value {
            None => NULL_CODE,
            Some(v) => match self.lookup.get(v) {
                Some(&c) => c,
                None => {
                    let c = self.dict.len() as u32;
                    self.dict.push(v.to_owned());
                    self.lookup.insert(v.to_owned(), c);
                    c
                }
            },
        };
        self.codes.push(code);
    }

    pub fn get(&self, row: usize) -> Option<&str> {
        match self.codes[row] {
            NULL_CODE => None,
            c => Some(self.dict[c as usize].as_str()),
        }
    }
}

pub fn canonical_name(raw: &str) -> &str {
    raw.strip_prefix("c_").unwrap_or(raw)
}

pub fn read_table(bytes: Bytes) -> Result<Table, ParquetError> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)?;
    let schema = builder.schema().clone();
    let reader = builder.with_batch_size(65_536).build()?;
    let mut columns: Vec<Col> = schema
        .fields()
        .iter()
        .map(|f| Col {
            name: canonical_name(f.name()).to_owned(),
            raw_name: f.name().clone(),
            data_type: f.data_type().to_string(),
            data: empty_data(f.data_type()),
        })
        .collect();
    let mut rows = 0;
    for batch in reader {
        let batch: RecordBatch = batch.map_err(|e| ParquetError::External(Box::new(e)))?;
        rows += batch.num_rows();
        for (col, array) in columns.iter_mut().zip(batch.columns()) {
            append(&mut col.data, array.as_ref());
        }
    }
    Ok(Table { rows, columns })
}

fn empty_data(data_type: &DataType) -> ColData {
    match data_type {
        DataType::Float64 | DataType::Float32 => ColData::F64(Vec::new()),
        DataType::Int64 | DataType::Int32 => ColData::I64(Vec::new()),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => {
            ColData::Str(DictStr::default())
        }
        DataType::Timestamp(unit, tz) => ColData::Ts {
            values: Vec::new(),
            unit: *unit,
            tz: tz.as_ref().map(|t| t.to_string()),
        },
        _ => ColData::Other,
    }
}

fn append(data: &mut ColData, array: &dyn Array) {
    match data {
        ColData::F64(v) => match array.data_type() {
            DataType::Float64 => {
                let a = array.as_primitive::<Float64Type>();
                v.extend((0..a.len()).map(|i| if a.is_null(i) { f64::NAN } else { a.value(i) }));
            }
            _ => {
                let a = array.as_primitive::<Float32Type>();
                v.extend((0..a.len()).map(|i| {
                    if a.is_null(i) {
                        f64::NAN
                    } else {
                        a.value(i) as f64
                    }
                }));
            }
        },
        ColData::I64(v) => match array.data_type() {
            DataType::Int64 => {
                let a = array.as_primitive::<Int64Type>();
                v.extend((0..a.len()).map(|i| if a.is_null(i) { NULL_I64 } else { a.value(i) }));
            }
            _ => {
                let a = array.as_primitive::<Int32Type>();
                v.extend((0..a.len()).map(|i| {
                    if a.is_null(i) {
                        NULL_I64
                    } else {
                        a.value(i) as i64
                    }
                }));
            }
        },
        ColData::Str(d) => match array.data_type() {
            DataType::Utf8 => {
                let a = array.as_string::<i32>();
                (0..a.len()).for_each(|i| d.push((!a.is_null(i)).then(|| a.value(i))));
            }
            DataType::LargeUtf8 => {
                let a = array.as_string::<i64>();
                (0..a.len()).for_each(|i| d.push((!a.is_null(i)).then(|| a.value(i))));
            }
            _ => {
                let a = array.as_string_view();
                (0..a.len()).for_each(|i| d.push((!a.is_null(i)).then(|| a.value(i))));
            }
        },
        ColData::Ts { values, unit, .. } => {
            macro_rules! ts {
                ($t:ty) => {{
                    let a = array.as_primitive::<$t>();
                    values.extend(
                        (0..a.len()).map(|i| if a.is_null(i) { NULL_I64 } else { a.value(i) }),
                    );
                }};
            }
            match unit {
                TimeUnit::Second => ts!(TimestampSecondType),
                TimeUnit::Millisecond => ts!(TimestampMillisecondType),
                TimeUnit::Microsecond => ts!(TimestampMicrosecondType),
                TimeUnit::Nanosecond => ts!(TimestampNanosecondType),
            }
        }
        ColData::Other => {}
    }
}

impl Table {
    pub fn col(&self, name: &str) -> Option<&Col> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn f64(&self, name: &str) -> Option<&[f64]> {
        match &self.col(name)?.data {
            ColData::F64(v) => Some(v),
            _ => None,
        }
    }

    pub fn i64(&self, name: &str) -> Option<&[i64]> {
        match &self.col(name)?.data {
            ColData::I64(v) => Some(v),
            _ => None,
        }
    }

    pub fn str(&self, name: &str) -> Option<&DictStr> {
        match &self.col(name)?.data {
            ColData::Str(d) => Some(d),
            _ => None,
        }
    }

    pub fn rows_equal(&self, a: usize, b: usize) -> bool {
        self.columns.iter().all(|c| match &c.data {
            ColData::F64(v) => v[a] == v[b] || (v[a].is_nan() && v[b].is_nan()),
            ColData::I64(v) => v[a] == v[b],
            ColData::Str(d) => d.codes[a] == d.codes[b],
            ColData::Ts { values, .. } => values[a] == values[b],
            ColData::Other => true,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Times {
    Parsed(Vec<NaiveDateTime>),
    Reformatted {
        times: Vec<NaiveDateTime>,
        format: &'static str,
    },
    Unparseable {
        example: String,
    },
    Missing,
}

const ISO_FORMATS: [&str; 4] = [
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S",
];
const ALT_FORMATS: [&str; 2] = ["%m/%d/%Y %H:%M:%S", "%m/%d/%Y %H:%M"];

pub fn new_york_times(col: &Col) -> Times {
    match &col.data {
        ColData::Ts { values, unit, tz } => {
            let mut cache: HashMap<i64, Option<NaiveDateTime>> = HashMap::new();
            let mut out = Vec::with_capacity(values.len());
            for &v in values {
                let t = *cache
                    .entry(v)
                    .or_insert_with(|| ts_to_new_york(v, *unit, tz.as_deref()));
                match t {
                    Some(t) => out.push(t),
                    None => {
                        return Times::Unparseable {
                            example: v.to_string(),
                        };
                    }
                }
            }
            Times::Parsed(out)
        }
        ColData::Str(d) => parse_text_times(d),
        _ => Times::Missing,
    }
}

fn parse_text_times(d: &DictStr) -> Times {
    if d.codes.contains(&NULL_CODE) {
        return Times::Unparseable {
            example: "null".to_owned(),
        };
    }
    let parse_all = |formats: &[&'static str]| -> Option<(Vec<NaiveDateTime>, &'static str)> {
        let mut used = formats[0];
        let mut parsed = Vec::with_capacity(d.dict.len());
        for text in &d.dict {
            let (t, f) = formats
                .iter()
                .find_map(|f| NaiveDateTime::parse_from_str(text, f).ok().map(|t| (t, *f)))?;
            used = f;
            parsed.push(t);
        }
        Some((parsed, used))
    };
    let expand = |parsed: Vec<NaiveDateTime>| d.codes.iter().map(|&c| parsed[c as usize]).collect();
    if let Some((parsed, _)) = parse_all(&ISO_FORMATS) {
        return Times::Parsed(expand(parsed));
    }
    if let Some((parsed, format)) = parse_all(&ALT_FORMATS) {
        return Times::Reformatted {
            times: expand(parsed),
            format,
        };
    }
    let example = d
        .dict
        .iter()
        .find(|t| {
            ISO_FORMATS
                .iter()
                .all(|f| NaiveDateTime::parse_from_str(t, f).is_err())
        })
        .cloned()
        .unwrap_or_default();
    Times::Unparseable { example }
}

fn ts_to_new_york(value: i64, unit: TimeUnit, tz: Option<&str>) -> Option<NaiveDateTime> {
    if value == NULL_I64 {
        return None;
    }
    let micros = match unit {
        TimeUnit::Second => value.checked_mul(1_000_000)?,
        TimeUnit::Millisecond => value.checked_mul(1_000)?,
        TimeUnit::Microsecond => value,
        TimeUnit::Nanosecond => value / 1_000,
    };
    let utc = DateTime::from_timestamp_micros(micros)?;
    match tz {
        // without a zone the value already is a wall-clock time
        None => Some(utc.naive_utc()),
        Some(_) => Some(
            utc.with_timezone(&chrono_tz::America::New_York)
                .naive_local(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Float64Array, StringArray, TimestampMicrosecondArray};
    use arrow_schema::{Field, Schema};
    use chrono::NaiveDate;
    use parquet::arrow::ArrowWriter;

    fn parquet_bytes(fields: Vec<Field>, arrays: Vec<ArrayRef>) -> Bytes {
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
        let mut buf = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut buf, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        Bytes::from(buf)
    }

    fn ny(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2023, 1, 10)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn reads_columns_and_strips_prefix() {
        let bytes = parquet_bytes(
            vec![
                Field::new("c_strike", DataType::Float64, true),
                Field::new("c_right", DataType::Utf8, true),
            ],
            vec![
                Arc::new(Float64Array::from(vec![Some(3900.0), None])),
                Arc::new(StringArray::from(vec!["CALL", "CALL"])),
            ],
        );
        let table = read_table(bytes).unwrap();
        assert_eq!(table.rows, 2);
        assert_eq!(table.columns[0].name, "strike");
        assert_eq!(table.columns[0].raw_name, "c_strike");
        assert!(table.f64("strike").unwrap()[1].is_nan());
        assert_eq!(table.str("right").unwrap().dict, ["CALL"]);
    }

    #[test]
    fn time_zone_timestamps_become_new_york_wall_clock() {
        let tz: Arc<str> = Arc::from("America/New_York");
        // 2023-01-10 14:30 utc is 09:30 in new york
        let values =
            TimestampMicrosecondArray::from(vec![1_673_361_000_000_000]).with_timezone(tz.clone());
        let bytes = parquet_bytes(
            vec![Field::new(
                "timestamp",
                DataType::Timestamp(TimeUnit::Microsecond, Some(tz)),
                true,
            )],
            vec![Arc::new(values)],
        );
        let table = read_table(bytes).unwrap();
        assert_eq!(
            new_york_times(table.col("timestamp").unwrap()),
            Times::Parsed(vec![ny(9, 30)])
        );
    }

    #[test]
    fn text_timestamps_in_iso_and_alternative_formats() {
        let iso = parquet_bytes(
            vec![Field::new("timestamp", DataType::Utf8, true)],
            vec![Arc::new(StringArray::from(vec![
                "2023-01-10T09:30:00.000",
                "2023-01-10T09:31:00.000",
            ]))],
        );
        let table = read_table(iso).unwrap();
        assert_eq!(
            new_york_times(table.col("timestamp").unwrap()),
            Times::Parsed(vec![ny(9, 30), ny(9, 31)])
        );

        let alt = parquet_bytes(
            vec![Field::new("timestamp", DataType::Utf8, true)],
            vec![Arc::new(StringArray::from(vec!["01/10/2023 09:30:00"]))],
        );
        let table = read_table(alt).unwrap();
        assert!(matches!(
            new_york_times(table.col("timestamp").unwrap()),
            Times::Reformatted { ref times, .. } if times == &vec![ny(9, 30)]
        ));

        let bad = parquet_bytes(
            vec![Field::new("timestamp", DataType::Utf8, true)],
            vec![Arc::new(StringArray::from(vec!["yesterday"]))],
        );
        let table = read_table(bad).unwrap();
        assert_eq!(
            new_york_times(table.col("timestamp").unwrap()),
            Times::Unparseable {
                example: "yesterday".into()
            }
        );
    }

    #[test]
    fn rows_equal_compares_every_column() {
        let bytes = parquet_bytes(
            vec![
                Field::new("bid", DataType::Float64, true),
                Field::new("right", DataType::Utf8, true),
            ],
            vec![
                Arc::new(Float64Array::from(vec![1.0, 1.0, 2.0])),
                Arc::new(StringArray::from(vec!["CALL", "CALL", "CALL"])),
            ],
        );
        let table = read_table(bytes).unwrap();
        assert!(table.rows_equal(0, 1));
        assert!(!table.rows_equal(0, 2));
    }
}
