//! Pure interaction helpers. Labels are display-only; identities stay native.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy)]
pub(crate) enum SelectionMove {
    Previous,
    Next,
    First,
    Last,
}

pub(crate) fn moved_index(
    current: Option<usize>,
    count: usize,
    movement: SelectionMove,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let last = count - 1;
    Some(match movement {
        SelectionMove::First => 0,
        SelectionMove::Last => last,
        SelectionMove::Previous => current.map_or(last, |index| index.min(last).saturating_sub(1)),
        SelectionMove::Next => current.map_or(0, |index| index.saturating_add(1).min(last)),
    })
}

pub(crate) fn local_breadcrumbs(path: &Path) -> Vec<(PathBuf, String)> {
    let mut current = PathBuf::new();
    let mut crumbs = Vec::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let label = match component {
            Component::RootDir => "/".into(),
            _ => component.as_os_str().to_string_lossy().into_owned(),
        };
        crumbs.push((current.clone(), label));
    }
    crumbs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_selection_is_bounded_and_handles_empty_snapshots() {
        assert_eq!(moved_index(None, 0, SelectionMove::Next), None);
        assert_eq!(moved_index(None, 3, SelectionMove::Next), Some(0));
        assert_eq!(moved_index(None, 3, SelectionMove::Previous), Some(2));
        assert_eq!(moved_index(Some(0), 3, SelectionMove::Previous), Some(0));
        assert_eq!(moved_index(Some(2), 3, SelectionMove::Next), Some(2));
        assert_eq!(
            moved_index(Some(usize::MAX), 3, SelectionMove::Next),
            Some(2)
        );
        assert_eq!(moved_index(Some(1), 3, SelectionMove::First), Some(0));
        assert_eq!(moved_index(Some(1), 3, SelectionMove::Last), Some(2));
    }

    #[test]
    fn breadcrumbs_capture_paths_not_reconstructed_labels() {
        let crumbs = local_breadcrumbs(Path::new("/Users/test/中文"));
        assert_eq!(crumbs[0].0, PathBuf::from("/"));
        assert_eq!(crumbs[2].0, PathBuf::from("/Users/test"));
        assert_eq!(
            crumbs[3],
            (PathBuf::from("/Users/test/中文"), "中文".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn lossy_breadcrumb_label_never_changes_native_path() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let path = PathBuf::from(std::ffi::OsString::from_vec(
            b"/parent/invalid-\xff".to_vec(),
        ));
        let crumbs = local_breadcrumbs(&path);
        let Some((actual, label)) = crumbs.last() else {
            panic!("missing breadcrumb");
        };
        assert_eq!(actual.as_os_str().as_bytes(), path.as_os_str().as_bytes());
        assert!(label.contains('\u{fffd}'));
    }
}
