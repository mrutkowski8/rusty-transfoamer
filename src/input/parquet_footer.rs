use std::fmt;
use std::fs::File;
use std::io;
use std::path::Path;

use parquet::arrow::parquet_to_arrow_schema;
use parquet::errors::ParquetError;
use parquet::file::metadata::ParquetMetaDataReader;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Column {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FooterSummary {
    pub rows: i64,
    pub row_groups: usize,
    pub created_by: Option<String>,
    pub columns: Vec<Column>,
}

#[derive(Debug)]
pub enum FooterError {
    Open(io::Error),
    Parquet(ParquetError),
}

impl fmt::Display for FooterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FooterError::Open(e) => write!(f, "cannot open file: {e}"),
            FooterError::Parquet(e) => write!(f, "cannot read Parquet footer: {e}"),
        }
    }
}

impl std::error::Error for FooterError {}

impl From<ParquetError> for FooterError {
    fn from(e: ParquetError) -> Self {
        FooterError::Parquet(e)
    }
}

pub fn read_footer(path: &Path) -> Result<FooterSummary, FooterError> {
    let file = File::open(path).map_err(FooterError::Open)?;
    footer_of(&file)
}

pub fn read_footer_bytes(bytes: &bytes::Bytes) -> Result<FooterSummary, FooterError> {
    footer_of(bytes)
}

fn footer_of<R: parquet::file::reader::ChunkReader>(
    reader: &R,
) -> Result<FooterSummary, FooterError> {
    // reads only the footer, never the data pages
    let metadata = ParquetMetaDataReader::new().parse_and_finish(reader)?;
    let file_metadata = metadata.file_metadata();
    // the key-value metadata carries the arrow schema written by pyarrow, time zones included
    let schema = parquet_to_arrow_schema(
        file_metadata.schema_descr(),
        file_metadata.key_value_metadata(),
    )?;
    let columns = schema
        .fields()
        .iter()
        .map(|field| Column {
            name: field.name().clone(),
            data_type: field.data_type().to_string(),
        })
        .collect();
    Ok(FooterSummary {
        rows: file_metadata.num_rows(),
        row_groups: metadata.num_row_groups(),
        created_by: file_metadata.created_by().map(str::to_owned),
        columns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use arrow_array::{
        ArrayRef, Float64Array, RecordBatch, StringArray, TimestampMicrosecondArray,
    };
    use arrow_schema::{DataType, Field, Schema, TimeUnit};
    use parquet::arrow::ArrowWriter;

    fn symbol() -> (Field, ArrayRef) {
        (
            Field::new("symbol", DataType::Utf8, true),
            Arc::new(StringArray::from(vec!["SPXW", "SPXW"])),
        )
    }

    fn strike() -> (Field, ArrayRef) {
        (
            Field::new("strike", DataType::Float64, true),
            Arc::new(Float64Array::from(vec![3900.0, 3905.0])),
        )
    }

    fn timestamp() -> (Field, ArrayRef) {
        let tz: Arc<str> = Arc::from("America/New_York");
        let field = Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Microsecond, Some(tz.clone())),
            true,
        );
        let values =
            TimestampMicrosecondArray::from(vec![1_673_361_000_000_000, 1_673_361_060_000_000])
                .with_timezone(tz);
        (field, Arc::new(values))
    }

    fn write_parquet(path: &Path, columns: Vec<(Field, ArrayRef)>) {
        let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = columns.into_iter().unzip();
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
        let mut writer = ArrowWriter::try_new(File::create(path).unwrap(), schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    fn name_type_set(summary: &FooterSummary) -> BTreeSet<(String, String)> {
        summary
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.data_type.clone()))
            .collect()
    }

    #[test]
    fn reads_rows_columns_and_time_zone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.parquet");
        write_parquet(&path, vec![symbol(), strike(), timestamp()]);

        let summary = read_footer(&path).unwrap();

        assert_eq!(summary.rows, 2);
        assert_eq!(summary.row_groups, 1);
        let names: Vec<&str> = summary.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["symbol", "strike", "timestamp"]);
        assert!(
            summary.columns[2].data_type.contains("America/New_York"),
            "{:?}",
            summary.columns[2]
        );
    }

    #[test]
    fn reordered_columns_keep_the_same_names_and_types() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.parquet");
        let b = dir.path().join("b.parquet");
        write_parquet(&a, vec![symbol(), strike(), timestamp()]);
        write_parquet(&b, vec![timestamp(), strike(), symbol()]);

        let first = read_footer(&a).unwrap();
        let second = read_footer(&b).unwrap();

        assert_ne!(first.columns, second.columns);
        assert_eq!(name_type_set(&first), name_type_set(&second));
    }

    #[test]
    fn empty_table_has_zero_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.parquet");
        let schema = Arc::new(Schema::new(vec![Field::new(
            "strike",
            DataType::Float64,
            true,
        )]));
        let writer = ArrowWriter::try_new(File::create(&path).unwrap(), schema, None).unwrap();
        writer.close().unwrap();

        assert_eq!(read_footer(&path).unwrap().rows, 0);
    }

    #[test]
    fn truncated_zero_byte_and_garbage_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.parquet");
        write_parquet(&good, vec![symbol(), strike()]);
        let bytes = std::fs::read(&good).unwrap();

        let truncated = dir.path().join("truncated.parquet");
        std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
        let zero = dir.path().join("zero.parquet");
        std::fs::write(&zero, b"").unwrap();
        let garbage = dir.path().join("garbage.parquet");
        std::fs::write(&garbage, b"this is not a parquet file at all").unwrap();

        for path in [truncated, zero, garbage] {
            assert!(
                matches!(read_footer(&path), Err(FooterError::Parquet(_))),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn missing_file_is_an_open_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_footer(&dir.path().join("missing.parquet"));
        assert!(matches!(result, Err(FooterError::Open(_))));
    }
}
