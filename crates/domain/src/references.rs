use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const MAX_DEVICE_KEY_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    #[error("device key must contain 1..={MAX_DEVICE_KEY_BYTES} bytes and no control characters")]
    InvalidDeviceKey,
    #[error("remote filename must be one nonempty component, not . or ..")]
    InvalidRemoteFilename,
    #[error("remote reference belongs to a different device")]
    DeviceMismatch,
    #[error("stale session: reference generation {reference:?}, active generation {active:?}")]
    StaleSession {
        reference: SessionGeneration,
        active: SessionGeneration,
    },
    #[error("byte size total exceeds u64")]
    SizeOverflow,
    #[error("session generation exhausted")]
    GenerationOverflow,
    #[error("selection is empty")]
    EmptySelection,
    #[error("selection mixes local and remote sources or different remote device/session/storage")]
    MixedSources,
}

/// An opaque stable device identifier, bounded in UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DeviceKey(String);

impl DeviceKey {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_DEVICE_KEY_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(DomainError::InvalidDeviceKey);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DeviceKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionGeneration(pub u64);

impl SessionGeneration {
    /// The session owner increments this at reconnect. Never wrap back to an old session.
    pub fn checked_next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::GenerationOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StorageId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteRef {
    device: DeviceKey,
    generation: SessionGeneration,
    storage: StorageId,
    object: ObjectId,
}

impl RemoteRef {
    pub fn new(
        device: DeviceKey,
        generation: SessionGeneration,
        storage: StorageId,
        object: ObjectId,
    ) -> Self {
        Self {
            device,
            generation,
            storage,
            object,
        }
    }
    pub fn device(&self) -> &DeviceKey {
        &self.device
    }
    pub fn generation(&self) -> SessionGeneration {
        self.generation
    }
    pub fn storage(&self) -> StorageId {
        self.storage
    }
    pub fn object(&self) -> ObjectId {
        self.object
    }

    pub fn validate_session(
        &self,
        device: &DeviceKey,
        active: SessionGeneration,
    ) -> Result<(), DomainError> {
        if &self.device != device {
            return Err(DomainError::DeviceMismatch);
        }
        if self.generation != active {
            return Err(DomainError::StaleSession {
                reference: self.generation,
                active,
            });
        }
        Ok(())
    }

    pub fn same_source(&self, other: &Self) -> bool {
        self.device == other.device
            && self.generation == other.generation
            && self.storage == other.storage
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntryRef {
    Local(PathBuf),
    Remote(RemoteRef),
}

/// A remote directory, including a storage root (`parent == None`). The
/// backend maps the explicit root to its protocol representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteDirectoryRef {
    device: DeviceKey,
    generation: SessionGeneration,
    storage: StorageId,
    parent: Option<ObjectId>,
}

impl RemoteDirectoryRef {
    pub fn new(
        device: DeviceKey,
        generation: SessionGeneration,
        storage: StorageId,
        parent: Option<ObjectId>,
    ) -> Self {
        Self {
            device,
            generation,
            storage,
            parent,
        }
    }
    pub fn device(&self) -> &DeviceKey {
        &self.device
    }
    pub fn generation(&self) -> SessionGeneration {
        self.generation
    }
    pub fn storage(&self) -> StorageId {
        self.storage
    }
    pub fn parent(&self) -> Option<ObjectId> {
        self.parent
    }

    pub fn validate_session(
        &self,
        device: &DeviceKey,
        active: SessionGeneration,
    ) -> Result<(), DomainError> {
        if &self.device != device {
            return Err(DomainError::DeviceMismatch);
        }
        if self.generation != active {
            return Err(DomainError::StaleSession {
                reference: self.generation,
                active,
            });
        }
        Ok(())
    }
}

impl From<RemoteRef> for RemoteDirectoryRef {
    fn from(reference: RemoteRef) -> Self {
        Self::new(
            reference.device,
            reference.generation,
            reference.storage,
            Some(reference.object),
        )
    }
}

/// A typed directory destination. Its actual existence/kind is verified by the backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DirectoryRef {
    Local(PathBuf),
    Remote(RemoteDirectoryRef),
}

impl EntryRef {
    pub fn validate_session(
        &self,
        device: &DeviceKey,
        active: SessionGeneration,
    ) -> Result<(), DomainError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Remote(reference) => reference.validate_session(device, active),
        }
    }
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote(_) => None,
        }
    }
}

impl DirectoryRef {
    pub fn validate_session(
        &self,
        device: &DeviceKey,
        active: SessionGeneration,
    ) -> Result<(), DomainError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Remote(reference) => reference.validate_session(device, active),
        }
    }
}

/// Platform-independent MTP filename: no host path parsing or normalization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RemoteFilename(String);

impl RemoteFilename {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\', '\0']) {
            return Err(DomainError::InvalidRemoteFilename);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RemoteFilename {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

pub fn checked_total_size(sizes: impl IntoIterator<Item = u64>) -> Result<u64, DomainError> {
    sizes.into_iter().try_fold(0_u64, |total, size| {
        total.checked_add(size).ok_or(DomainError::SizeOverflow)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn remote() -> RemoteRef {
        RemoteRef::new(
            DeviceKey::new("phone").unwrap(),
            SessionGeneration(3),
            StorageId(7),
            ObjectId(9),
        )
    }

    #[test]
    fn device_key_accepts_nonempty_identifier() {
        assert_eq!(DeviceKey::new("usb:设备").unwrap().as_str(), "usb:设备");
    }
    #[test]
    fn device_key_rejects_empty() {
        assert_eq!(DeviceKey::new(""), Err(DomainError::InvalidDeviceKey));
    }
    #[test]
    fn device_key_bounds_bytes() {
        assert!(DeviceKey::new("a".repeat(MAX_DEVICE_KEY_BYTES)).is_ok());
        assert!(DeviceKey::new("a".repeat(MAX_DEVICE_KEY_BYTES + 1)).is_err());
        assert!(DeviceKey::new("界".repeat(86)).is_err());
    }
    #[test]
    fn device_key_rejects_controls() {
        for value in ["a\0b", "a\nb", "a\rb"] {
            assert!(DeviceKey::new(value).is_err());
        }
    }
    #[test]
    fn device_key_deserialization_is_validated() {
        assert!(serde_json::from_str::<DeviceKey>("\"\"").is_err());
    }
    #[test]
    fn device_key_roundtrips() {
        let key = DeviceKey::new("phone").unwrap();
        assert_eq!(
            serde_json::from_str::<DeviceKey>(&serde_json::to_string(&key).unwrap()).unwrap(),
            key
        );
    }
    #[test]
    fn session_generation_never_wraps() {
        assert_eq!(
            SessionGeneration(u64::MAX).checked_next(),
            Err(DomainError::GenerationOverflow)
        );
    }
    #[test]
    fn session_generation_increments() {
        assert_eq!(
            SessionGeneration(3).checked_next(),
            Ok(SessionGeneration(4))
        );
    }
    #[test]
    fn reference_accepts_current_session() {
        let r = remote();
        assert!(r.validate_session(r.device(), SessionGeneration(3)).is_ok());
    }
    #[test]
    fn reference_rejects_old_session() {
        let r = remote();
        assert!(matches!(
            r.validate_session(r.device(), SessionGeneration(4)),
            Err(DomainError::StaleSession { .. })
        ));
    }
    #[test]
    fn reference_rejects_other_device() {
        assert_eq!(
            remote().validate_session(&DeviceKey::new("other").unwrap(), SessionGeneration(3)),
            Err(DomainError::DeviceMismatch)
        );
    }
    #[test]
    fn directory_rejects_stale_session() {
        let r = remote();
        let d = DirectoryRef::Remote(r.clone().into());
        assert!(
            d.validate_session(r.device(), SessionGeneration(4))
                .is_err()
        );
    }
    #[test]
    fn remote_storage_root_is_explicit() {
        let root = RemoteDirectoryRef::new(
            remote().device().clone(),
            SessionGeneration(3),
            StorageId(7),
            None,
        );
        assert_eq!(root.parent(), None);
        assert_eq!(root.storage(), StorageId(7));
        assert!(
            root.validate_session(remote().device(), SessionGeneration(3))
                .is_ok()
        );
    }
    #[test]
    fn remote_root_rejects_stale_or_wrong_session() {
        let root = RemoteDirectoryRef::new(
            remote().device().clone(),
            SessionGeneration(3),
            StorageId(7),
            None,
        );
        assert!(matches!(
            root.validate_session(remote().device(), SessionGeneration(4)),
            Err(DomainError::StaleSession { .. })
        ));
        assert_eq!(
            root.validate_session(&DeviceKey::new("other").unwrap(), SessionGeneration(3)),
            Err(DomainError::DeviceMismatch)
        );
    }
    #[test]
    fn remote_directory_from_object_keeps_parent() {
        let directory = RemoteDirectoryRef::from(remote());
        assert_eq!(directory.parent(), Some(ObjectId(9)));
        assert_eq!(directory.generation(), SessionGeneration(3));
    }
    #[test]
    fn remote_root_roundtrips_without_sentinel() {
        let root = DirectoryRef::Remote(RemoteDirectoryRef::new(
            remote().device().clone(),
            SessionGeneration(3),
            StorageId(7),
            None,
        ));
        assert_eq!(
            serde_json::from_str::<DirectoryRef>(&serde_json::to_string(&root).unwrap()).unwrap(),
            root
        );
    }
    #[test]
    fn entry_rejects_stale_session() {
        let r = remote();
        let e = EntryRef::Remote(r.clone());
        assert!(
            e.validate_session(r.device(), SessionGeneration(4))
                .is_err()
        );
    }
    #[test]
    fn local_reference_needs_no_remote_session() {
        assert!(
            EntryRef::Local("/tmp".into())
                .validate_session(remote().device(), SessionGeneration(99))
                .is_ok()
        );
    }
    #[test]
    fn remote_filename_rejects_unsafe_components() {
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            "/absolute",
            "\\absolute",
        ] {
            assert!(RemoteFilename::new(name).is_err(), "{name:?}");
        }
    }
    #[test]
    fn remote_filename_accepts_unicode_and_spaces() {
        assert_eq!(
            RemoteFilename::new("照片 01.jpg").unwrap().as_str(),
            "照片 01.jpg"
        );
    }
    #[test]
    fn remote_filename_deserialization_is_validated() {
        assert!(serde_json::from_str::<RemoteFilename>("\"../bad\"").is_err());
    }
    #[test]
    fn sizes_cover_empty_and_exact_max() {
        assert_eq!(checked_total_size([]), Ok(0));
        assert_eq!(checked_total_size([u64::MAX - 1, 1]), Ok(u64::MAX));
    }
    #[test]
    fn sizes_reject_overflow() {
        assert_eq!(
            checked_total_size([u64::MAX, 1]),
            Err(DomainError::SizeOverflow)
        );
    }
}
