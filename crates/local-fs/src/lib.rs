//! Read-only local filesystem adapter. Call from a background thread, never Render.
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: OsString,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub byte_size: Option<u64>,
    pub modified_at: Option<SystemTime>,
    /// Metadata errors are visible, not represented as an empty/zero-sized file.
    pub metadata_error: Option<String>,
}

#[derive(Debug, Error)]
#[error("cannot read local directory: {source}")]
pub struct BrowseError {
    #[source]
    pub source: io::Error,
}

/// Preserve native filenames; display conversion must not become an operation path.
/// Symlinks are listed as links and are never followed for metadata.
pub fn read_directory(path: &Path, show_hidden: bool) -> Result<Vec<LocalEntry>, BrowseError> {
    let directory = fs::read_dir(path).map_err(|source| BrowseError { source })?;
    let mut entries = Vec::new();
    for entry in directory {
        let entry = entry.map_err(|source| BrowseError { source })?;
        let name = entry.file_name();
        if !show_hidden && name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let path = entry.path();
        let (kind, byte_size, modified_at, metadata_error) = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let kind = if metadata.is_symlink() {
                    EntryKind::Symlink
                } else if metadata.is_dir() {
                    EntryKind::Directory
                } else if metadata.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                let size = (kind == EntryKind::File).then_some(metadata.len());
                (kind, size, metadata.modified().ok(), None)
            }
            Err(error) => (EntryKind::Unknown, None, None, Some(error.to_string())),
        };
        entries.push(LocalEntry {
            name,
            path,
            kind,
            byte_size,
            modified_at,
            metadata_error,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Directory, &a.name).cmp(&(b.kind != EntryKind::Directory, &b.name))
    });
    Ok(entries)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mtp-native-local-test-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            assert!(
                self.0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mtp-native-local-test-")
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn directories_first_and_native_names() {
        let d = TestDir::new();
        fs::write(d.0.join("中文.txt"), b"abc").unwrap();
        fs::create_dir(d.0.join("z-dir")).unwrap();
        let entries = read_directory(&d.0, false).unwrap();
        assert_eq!(entries[0].kind, EntryKind::Directory);
        assert_eq!(entries[1].name, OsString::from("中文.txt"));
        assert_eq!(entries[1].byte_size, Some(3));
    }
    #[test]
    fn hidden_setting_is_explicit() {
        let d = TestDir::new();
        fs::write(d.0.join(".hidden"), b"").unwrap();
        assert!(read_directory(&d.0, false).unwrap().is_empty());
        assert_eq!(read_directory(&d.0, true).unwrap().len(), 1);
    }
    #[test]
    fn missing_directory_is_error_not_empty() {
        let d = TestDir::new();
        assert!(read_directory(&d.0.join("missing"), false).is_err());
    }
    #[test]
    fn empty_file_has_known_zero_size() {
        let d = TestDir::new();
        fs::write(d.0.join("empty"), b"").unwrap();
        assert_eq!(read_directory(&d.0, false).unwrap()[0].byte_size, Some(0));
    }
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_not_followed() {
        let d = TestDir::new();
        std::os::unix::fs::symlink("absent", d.0.join("link")).unwrap();
        let entry = read_directory(&d.0, false).unwrap().remove(0);
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert_eq!(entry.byte_size, None);
        assert!(entry.metadata_error.is_none());
    }
    // APFS rejects invalid UTF-8 names. Actual filesystem coverage is Linux-only.
    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_utf8_name_is_preserved() {
        use std::os::unix::ffi::OsStringExt;
        let d = TestDir::new();
        let name = OsString::from_vec(vec![b'a', 255]);
        fs::write(d.0.join(&name), b"").unwrap();
        assert_eq!(read_directory(&d.0, false).unwrap()[0].name, name);
    }
}
