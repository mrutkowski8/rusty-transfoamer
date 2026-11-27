use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    write_atomic(path, |w| {
        serde_json::to_writer_pretty(&mut *w, value)?;
        w.write_all(b"\n")
    })
}

pub fn write_text_atomic(path: &Path, text: &str) -> io::Result<()> {
    write_atomic(path, |w| w.write_all(text.as_bytes()))
}

fn write_atomic(
    path: &Path,
    fill: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    {
        let mut writer = BufWriter::new(fs::File::create(&tmp)?);
        fill(&mut writer)?;
        writer.flush()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_json_and_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step0").join("report.json");

        write_json_atomic(&path, &serde_json::json!({"step": 0})).unwrap();
        write_json_atomic(&path, &serde_json::json!({"step": 0, "again": true})).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["again"], true);
        assert!(!dir.path().join("step0").join("report.json.tmp").exists());
    }

    #[test]
    fn writes_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_text_atomic(&path, "line\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "line\n");
    }
}
