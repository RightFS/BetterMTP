//! Read-only directory state. Native paths are never reconstructed from display text.

use std::path::PathBuf;

use mtp_local_fs::LocalEntry;

pub(crate) struct LocalBrowser {
    pub(crate) path: PathBuf,
    pub(crate) entries: Vec<LocalEntry>,
    pub(crate) selected: Option<PathBuf>,
    pub(crate) show_hidden: bool,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    revision: u64,
    exhausted: bool,
}

impl LocalBrowser {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            entries: Vec::new(),
            selected: None,
            show_hidden: false,
            loading: false,
            error: None,
            revision: 0,
            exhausted: false,
        }
    }

    /// Clear the old snapshot immediately so it cannot be used with a new address.
    pub(crate) fn begin(&mut self, path: PathBuf) -> Result<u64, &'static str> {
        let Some(revision) = self.revision.checked_add(1) else {
            const ERROR: &str = "目录请求编号已耗尽，请重新启动应用。";
            self.exhausted = true;
            self.loading = false;
            self.entries.clear();
            self.selected = None;
            self.error = Some(ERROR.into());
            return Err(ERROR);
        };
        self.revision = revision;
        self.path = path;
        self.entries.clear();
        self.selected = None;
        self.loading = true;
        self.error = None;
        Ok(revision)
    }

    /// Both late successes and late failures must leave the current request untouched.
    pub(crate) fn complete(
        &mut self,
        revision: u64,
        result: Result<Vec<LocalEntry>, String>,
    ) -> bool {
        if self.exhausted || revision != self.revision {
            return false;
        }
        self.loading = false;
        match result {
            Ok(entries) => self.entries = entries,
            Err(error) => self.error = Some(error),
        }
        true
    }

    pub(crate) fn snapshot_revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn selected_entry(&self) -> Option<&LocalEntry> {
        let selected = self.selected.as_ref()?;
        self.entries.iter().find(|entry| &entry.path == selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(browser: &mut LocalBrowser, path: &str) -> u64 {
        match browser.begin(PathBuf::from(path)) {
            Ok(revision) => revision,
            Err(error) => panic!("unexpected request failure: {error}"),
        }
    }

    #[test]
    fn late_success_does_not_clear_current_loading() {
        let mut browser = LocalBrowser::new(PathBuf::from("/initial"));
        let old = begin(&mut browser, "/old");
        let current = begin(&mut browser, "/current");
        assert!(!browser.complete(old, Ok(Vec::new())));
        assert!(browser.loading);
        assert_eq!(browser.path, PathBuf::from("/current"));
        assert!(browser.complete(current, Ok(Vec::new())));
        assert!(!browser.loading);
        assert!(browser.error.is_none());
    }

    #[test]
    fn late_failure_does_not_replace_current_result() {
        let mut browser = LocalBrowser::new(PathBuf::from("/initial"));
        let old = begin(&mut browser, "/old");
        let current = begin(&mut browser, "/current");
        assert!(browser.complete(current, Ok(Vec::new())));
        assert!(!browser.complete(old, Err("old failure".into())));
        assert!(browser.error.is_none());
        assert!(!browser.loading);
    }

    #[test]
    fn new_request_clears_selection_and_error() {
        let mut browser = LocalBrowser::new(PathBuf::from("/initial"));
        let request = begin(&mut browser, "/missing");
        assert!(browser.complete(request, Err("permission denied".into())));
        browser.selected = Some(PathBuf::from("/initial/file"));
        begin(&mut browser, "/retry");
        assert!(browser.selected.is_none());
        assert!(browser.error.is_none());
        assert!(browser.loading);
    }

    #[test]
    fn revision_exhaustion_is_terminal_and_never_reuses_a_revision() {
        let mut browser = LocalBrowser::new(PathBuf::from("/initial"));
        browser.revision = u64::MAX - 1;
        let last = begin(&mut browser, "/last");
        assert_eq!(last, u64::MAX);
        assert!(browser.loading);
        browser.selected = Some(PathBuf::from("/last/file"));

        let exhausted = browser.begin(PathBuf::from("/must-not-start"));
        assert!(exhausted.is_err());
        assert_eq!(browser.revision, u64::MAX);
        assert_eq!(browser.path, PathBuf::from("/last"));
        assert!(browser.exhausted);
        assert!(!browser.loading);
        assert!(browser.selected.is_none());
        assert!(browser.entries.is_empty());
        assert_eq!(browser.error.as_deref(), exhausted.err());

        let terminal_error = browser.error.clone();
        assert!(!browser.complete(last, Ok(Vec::new())));
        assert!(!browser.complete(last, Err("late failure".into())));
        assert_eq!(browser.error, terminal_error);
        assert!(browser.begin(PathBuf::from("/retry")).is_err());
        assert_eq!(browser.revision, u64::MAX);
        assert_eq!(browser.path, PathBuf::from("/last"));
        assert!(!browser.loading);
    }
}
