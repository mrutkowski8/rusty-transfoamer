use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FolderListing {
    pub files: Vec<PathBuf>,
    pub empty_dirs: Vec<PathBuf>,
}

pub fn list_folder(root: &Path) -> io::Result<FolderListing> {
    let mut listing = FolderListing::default();
    walk(root, root, &mut listing)?;
    listing.files.sort();
    listing.empty_dirs.sort();
    Ok(listing)
}

fn walk(root: &Path, dir: &Path, listing: &mut FolderListing) -> io::Result<()> {
    let mut is_empty = true;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        is_empty = false;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(root, &path, listing)?;
        } else {
            listing.files.push(relative(root, &path));
        }
    }
    if is_empty && dir != root {
        listing.empty_dirs.push(relative(root, dir));
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_files_and_empty_dirs_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("quote_1m")).unwrap();
        fs::create_dir_all(root.join("quote_stream")).unwrap();
        fs::write(root.join("quote_1m").join("b.parquet"), b"x").unwrap();
        fs::write(root.join("quote_1m").join("a.parquet"), b"x").unwrap();
        fs::write(root.join("desktop.ini"), b"x").unwrap();

        let listing = list_folder(root).unwrap();

        assert_eq!(
            listing.files,
            vec![
                PathBuf::from("desktop.ini"),
                Path::new("quote_1m").join("a.parquet"),
                Path::new("quote_1m").join("b.parquet"),
            ]
        );
        assert_eq!(listing.empty_dirs, vec![PathBuf::from("quote_stream")]);
    }

    #[test]
    fn empty_root_is_not_reported_as_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let listing = list_folder(dir.path()).unwrap();
        assert_eq!(listing, FolderListing::default());
    }
}
