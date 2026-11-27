use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use zip::ZipArchive;

use super::folder::{FolderListing, list_folder};

#[derive(Debug, Clone)]
pub enum Source {
    Folder(PathBuf),
    Zip(ZipSource),
}

#[derive(Debug, Clone)]
pub struct ZipSource {
    path: PathBuf,
    entries: Vec<ZipEntry>,
}

#[derive(Debug, Clone)]
struct ZipEntry {
    archive_name: String,
    rel: String,
    is_dir: bool,
    size: u64,
}

impl Source {
    pub fn open(path: &Path) -> io::Result<Source> {
        if path.is_dir() {
            return Ok(Source::Folder(path.to_path_buf()));
        }
        if path.is_file()
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            return Ok(Source::Zip(ZipSource::open(path)?));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "input is neither a folder nor a .zip file: {}",
                path.display()
            ),
        ))
    }

    pub fn path(&self) -> &Path {
        match self {
            Source::Folder(p) => p,
            Source::Zip(z) => &z.path,
        }
    }

    pub fn top_dirs(&self) -> io::Result<Vec<String>> {
        match self {
            Source::Folder(root) => dir_names(root),
            Source::Zip(z) => Ok(z.children("")),
        }
    }

    pub fn sub_dirs(&self, root: &str) -> io::Result<Vec<String>> {
        match self {
            Source::Folder(base) => dir_names(&base.join(root)),
            Source::Zip(z) => Ok(z.children(root)),
        }
    }

    pub fn has_dir(&self, root: &str, date: &str) -> bool {
        match self {
            Source::Folder(base) => base.join(root).join(date).is_dir(),
            Source::Zip(z) => {
                let prefix = format!("{root}/{date}/");
                z.entries.iter().any(|e| e.rel.starts_with(&prefix))
            }
        }
    }

    pub fn day_path(&self, root: &str, date: &str) -> String {
        let base = self.path().to_string_lossy().replace('\\', "/");
        match self {
            Source::Folder(_) => format!("{base}/{root}/{date}"),
            Source::Zip(_) => format!("{base}!/{root}/{date}"),
        }
    }

    pub fn list_day(&self, root: &str, date: &str) -> io::Result<FolderListing> {
        match self {
            Source::Folder(base) => list_folder(&base.join(root).join(date)),
            Source::Zip(z) => Ok(z.list(&format!("{root}/{date}/"))),
        }
    }

    pub fn reader(&self) -> io::Result<DayReader> {
        match self {
            Source::Folder(base) => Ok(DayReader::Folder(base.clone())),
            Source::Zip(z) => Ok(DayReader::Zip {
                archive: ZipArchive::new(File::open(&z.path)?).map_err(io::Error::other)?,
                source: z.clone(),
            }),
        }
    }
}

pub enum DayReader {
    Folder(PathBuf),
    Zip {
        archive: ZipArchive<File>,
        source: ZipSource,
    },
}

impl DayReader {
    pub fn size(&self, rel: &str) -> Option<u64> {
        match self {
            DayReader::Folder(base) => std::fs::metadata(base.join(rel)).ok().map(|m| m.len()),
            DayReader::Zip { source, .. } => source.entry(rel).map(|e| e.size),
        }
    }

    pub fn read(&mut self, rel: &str) -> io::Result<Bytes> {
        match self {
            DayReader::Folder(base) => Ok(Bytes::from(std::fs::read(base.join(rel))?)),
            DayReader::Zip { archive, source } => {
                let entry = source
                    .entry(rel)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))?;
                let mut file = archive
                    .by_name(&entry.archive_name)
                    .map_err(io::Error::other)?;
                let mut buf = Vec::with_capacity(entry.size as usize);
                file.read_to_end(&mut buf)?;
                Ok(Bytes::from(buf))
            }
        }
    }
}

impl ZipSource {
    fn open(path: &Path) -> io::Result<ZipSource> {
        let mut archive = ZipArchive::new(File::open(path)?).map_err(io::Error::other)?;
        let mut raw = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let file = archive.by_index(i).map_err(io::Error::other)?;
            let name = file.name().map_err(io::Error::other)?.replace('\\', "/");
            raw.push((name, file.is_dir(), file.size()));
        }
        let prefix = common_wrapper(&raw.iter().map(|r| r.0.as_str()).collect::<Vec<_>>());
        let entries = raw
            .into_iter()
            .filter_map(|(name, is_dir, size)| {
                let rel = name.strip_prefix(&prefix)?.trim_end_matches('/').to_owned();
                (!rel.is_empty()).then_some(ZipEntry {
                    archive_name: name,
                    rel,
                    is_dir,
                    size,
                })
            })
            .collect();
        Ok(ZipSource {
            path: path.to_path_buf(),
            entries,
        })
    }

    fn entry(&self, rel: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| !e.is_dir && e.rel == rel)
    }

    fn children(&self, parent: &str) -> Vec<String> {
        let prefix = if parent.is_empty() {
            String::new()
        } else {
            format!("{parent}/")
        };
        let mut names = BTreeSet::new();
        for e in &self.entries {
            if let Some(rest) = e.rel.strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((dir, _)) => {
                        names.insert(dir.to_owned());
                    }
                    None if e.is_dir => {
                        names.insert(rest.to_owned());
                    }
                    None => {}
                }
            }
        }
        names.into_iter().collect()
    }

    fn list(&self, prefix: &str) -> FolderListing {
        let mut listing = FolderListing::default();
        let mut dirs = BTreeSet::new();
        let mut parents = BTreeSet::new();
        for e in &self.entries {
            let Some(rest) = e.rel.strip_prefix(prefix) else {
                continue;
            };
            if e.is_dir {
                dirs.insert(rest.to_owned());
            } else {
                listing.files.push(PathBuf::from(rest));
            }
            let mut p = Path::new(rest).parent();
            while let Some(parent) = p.filter(|p| !p.as_os_str().is_empty()) {
                parents.insert(parent.to_string_lossy().replace('\\', "/"));
                p = parent.parent();
            }
        }
        for dir in dirs {
            let has_child = self
                .entries
                .iter()
                .any(|e| e.rel.starts_with(&format!("{prefix}{dir}/")));
            if !has_child {
                listing.empty_dirs.push(PathBuf::from(dir));
            }
        }
        listing.files.sort();
        listing.empty_dirs.sort();
        listing
    }
}

/// a zip made from the pack folder wraps everything in it, e.g. `day_range_pack/SPX/...`
fn common_wrapper(names: &[&str]) -> String {
    let firsts: BTreeSet<&str> = names.iter().filter_map(|n| n.split('/').next()).collect();
    if firsts.len() != 1 {
        return String::new();
    }
    let first = *firsts.iter().next().unwrap();
    let seconds: BTreeSet<&str> = names
        .iter()
        .filter_map(|n| n.split('/').nth(1))
        .filter(|s| !s.is_empty())
        .collect();
    let looks_like_roots = !seconds.is_empty()
        && seconds
            .iter()
            .all(|s| s.chars().all(|c| c.is_ascii_uppercase()));
    if looks_like_roots {
        format!("{first}/")
    } else {
        String::new()
    }
}

fn dir_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            match data {
                Some(bytes) => {
                    writer.start_file(*name, options).unwrap();
                    writer.write_all(bytes).unwrap();
                }
                None => writer.add_directory(*name, options).unwrap(),
            }
        }
        writer.finish().unwrap();
    }

    #[test]
    fn zip_with_wrapper_folder_lists_like_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pack.zip");
        make_zip(
            &path,
            &[
                (
                    "pack/SPXW/2023-01-10/quote_1m/2023-01-10.parquet",
                    Some(b"abc"),
                ),
                ("pack/SPXW/2023-01-10/desktop.ini", Some(b"x")),
                ("pack/SPXW/2026-07-17/quote_stream/", None),
                (
                    "pack/VIX/2023-01-10/quote_5m/2023-01-18.parquet",
                    Some(b"d"),
                ),
            ],
        );
        let source = Source::open(&path).unwrap();
        assert_eq!(source.top_dirs().unwrap(), ["SPXW", "VIX"]);
        assert_eq!(
            source.sub_dirs("SPXW").unwrap(),
            ["2023-01-10", "2026-07-17"]
        );
        let day = source.list_day("SPXW", "2023-01-10").unwrap();
        assert_eq!(
            day.files,
            [
                PathBuf::from("desktop.ini"),
                PathBuf::from("quote_1m/2023-01-10.parquet")
            ]
        );
        let empty = source.list_day("SPXW", "2026-07-17").unwrap();
        assert_eq!(empty.empty_dirs, [PathBuf::from("quote_stream")]);
        let mut reader = source.reader().unwrap();
        let rel = "SPXW/2023-01-10/quote_1m/2023-01-10.parquet";
        assert_eq!(reader.read(rel).unwrap().as_ref(), b"abc");
        assert_eq!(reader.size(rel), Some(3));
    }

    #[test]
    fn zip_without_wrapper_is_read_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pack.zip");
        make_zip(
            &path,
            &[("SPX/2023-01-10/open_interest.parquet", Some(b"x"))],
        );
        let source = Source::open(&path).unwrap();
        assert_eq!(source.top_dirs().unwrap(), ["SPX"]);
        assert!(source.has_dir("SPX", "2023-01-10"));
    }
}
