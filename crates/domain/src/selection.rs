use serde::Serialize;

use crate::{DirectoryRef, DomainError, EntryRef};

/// Owned clipboard selection. Private fields and validated construction prevent
/// mutation or deserialization from bypassing the single-source invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopyBuffer {
    selection: Vec<EntryRef>,
}

impl CopyBuffer {
    pub fn capture(selection: &[EntryRef]) -> Result<Self, DomainError> {
        let first = selection.first().ok_or(DomainError::EmptySelection)?;
        for entry in selection {
            let same_source = match (first, entry) {
                (EntryRef::Local(_), EntryRef::Local(_)) => true,
                (EntryRef::Remote(first), EntryRef::Remote(entry)) => first.same_source(entry),
                _ => false,
            };
            if !same_source {
                return Err(DomainError::MixedSources);
            }
        }
        Ok(Self {
            selection: selection.to_vec(),
        })
    }

    pub fn selection(&self) -> &[EntryRef] {
        &self.selection
    }

    /// Paste binds both selection and destination at this instant. Later UI
    /// navigation/clipboard changes cannot redirect an already queued transfer.
    pub fn snapshot(&self, destination: &DirectoryRef) -> CopySnapshot {
        CopySnapshot {
            selection: self.selection.clone(),
            destination: destination.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopySnapshot {
    selection: Vec<EntryRef>,
    destination: DirectoryRef,
}

impl CopySnapshot {
    pub fn selection(&self) -> &[EntryRef] {
        &self.selection
    }
    pub fn destination(&self) -> &DirectoryRef {
        &self.destination
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{DeviceKey, ObjectId, RemoteDirectoryRef, RemoteRef, SessionGeneration, StorageId};

    #[test]
    fn paste_captures_remote_storage_root() {
        let buffer = CopyBuffer::capture(&[remote("a", 1, 1, 1)]).unwrap();
        let destination = DirectoryRef::Remote(RemoteDirectoryRef::new(
            DeviceKey::new("a").unwrap(),
            SessionGeneration(1),
            StorageId(1),
            None,
        ));
        let snapshot = buffer.snapshot(&destination);
        assert_eq!(snapshot.destination(), &destination);
        match snapshot.destination() {
            DirectoryRef::Remote(root) => assert_eq!(root.parent(), None),
            DirectoryRef::Local(_) => panic!("expected captured remote root"),
        }
        assert!(
            snapshot
                .destination()
                .validate_session(&DeviceKey::new("a").unwrap(), SessionGeneration(2))
                .is_err()
        );
    }

    fn remote(device: &str, generation: u64, storage: u32, object: u32) -> EntryRef {
        EntryRef::Remote(RemoteRef::new(
            DeviceKey::new(device).unwrap(),
            SessionGeneration(generation),
            StorageId(storage),
            ObjectId(object),
        ))
    }
    #[test]
    fn rejects_empty_selection() {
        assert_eq!(CopyBuffer::capture(&[]), Err(DomainError::EmptySelection));
    }
    #[test]
    fn accepts_multiple_objects_of_same_source() {
        assert!(CopyBuffer::capture(&[remote("a", 1, 1, 1), remote("a", 1, 1, 2)]).is_ok());
    }
    #[test]
    fn rejects_mixed_devices() {
        assert_eq!(
            CopyBuffer::capture(&[remote("a", 1, 1, 1), remote("b", 1, 1, 2)]),
            Err(DomainError::MixedSources)
        );
    }
    #[test]
    fn rejects_mixed_generations() {
        assert_eq!(
            CopyBuffer::capture(&[remote("a", 1, 1, 1), remote("a", 2, 1, 2)]),
            Err(DomainError::MixedSources)
        );
    }
    #[test]
    fn rejects_mixed_storages() {
        assert_eq!(
            CopyBuffer::capture(&[remote("a", 1, 1, 1), remote("a", 1, 2, 2)]),
            Err(DomainError::MixedSources)
        );
    }
    #[test]
    fn rejects_mixed_local_remote() {
        assert_eq!(
            CopyBuffer::capture(&[EntryRef::Local("a".into()), remote("a", 1, 1, 1)]),
            Err(DomainError::MixedSources)
        );
    }
    #[test]
    fn accepts_local_selection() {
        assert!(
            CopyBuffer::capture(&[EntryRef::Local("a".into()), EntryRef::Local("b".into())])
                .is_ok()
        );
    }
    #[test]
    fn selection_is_owned_snapshot() {
        let mut selection = vec![remote("a", 1, 1, 1)];
        let buffer = CopyBuffer::capture(&selection).unwrap();
        selection[0] = remote("b", 2, 2, 2);
        assert_eq!(buffer.selection(), &[remote("a", 1, 1, 1)]);
    }
    #[test]
    fn paste_captures_destination_and_selection() {
        let buffer = CopyBuffer::capture(&[remote("a", 1, 1, 1)]).unwrap();
        let mut destination = DirectoryRef::Local("first".into());
        let snapshot = buffer.snapshot(&destination);
        destination = DirectoryRef::Local("second".into());
        let replacement = CopyBuffer::capture(&[remote("b", 2, 2, 2)]).unwrap();
        assert_ne!(snapshot.destination(), &destination);
        assert_ne!(snapshot.selection(), replacement.selection());
        assert_eq!(snapshot.destination(), &DirectoryRef::Local("first".into()));
    }
}
