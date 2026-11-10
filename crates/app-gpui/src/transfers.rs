//! Independent single-file UI task state. Snapshot identities never come from labels.

use std::path::{Path, PathBuf};

use mtp_application::TransferReceipt;
use mtp_backend_api::{FileConflictPolicy, TransferDirection};
use mtp_domain::{RemoteDirectoryRef, RemoteRef};
use mtp_local_fs::{EntryKind, LocalEntry};

#[derive(Clone)]
pub(crate) enum TransferPlan {
    Upload {
        source: PathBuf,
        requested_name: String,
        destination: RemoteDirectoryRef,
        target_label: String,
        cache: PathBuf,
    },
    Download {
        source: RemoteRef,
        source_name: String,
        source_address: String,
        destination: PathBuf,
        policy: FileConflictPolicy,
    },
}

impl TransferPlan {
    pub(crate) fn upload(
        entry: &LocalEntry,
        destination: RemoteDirectoryRef,
        target_label: String,
        cache: PathBuf,
    ) -> Result<Self, &'static str> {
        if entry.kind != EntryKind::File {
            return Err("当前仅支持普通文件上传；目录和符号链接不支持，不会递归。");
        }
        if entry.metadata_error.is_some() {
            return Err("源文件元数据读取失败；不能开始上传。");
        }
        let requested_name = upload_name(&entry.name)?;
        Ok(Self::Upload {
            source: entry.path.clone(),
            requested_name,
            destination,
            target_label,
            cache,
        })
    }

    pub(crate) fn target_label(&self) -> String {
        match self {
            Self::Upload {
                target_label,
                source,
                ..
            } => format!(
                "捕获上传目标: {target_label} · 源: {}",
                source.to_string_lossy()
            ),
            Self::Download {
                destination,
                source_name,
                source_address,
                ..
            } => format!(
                "捕获下载目标: {} · 源: {}{}{}",
                destination.to_string_lossy(),
                source_address.trim_end_matches('/'),
                if source_address.is_empty() { "" } else { "/" },
                source_name
            ),
        }
    }
}

fn upload_name(name: &std::ffi::OsStr) -> Result<String, &'static str> {
    let name = name
        .to_str()
        .ok_or("上传文件名不是有效 UTF-8；不以 lossy 名称传输。")?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name
            .chars()
            .any(|character| character.is_control() || character == '/' || character == '\\')
    {
        return Err("上传文件名不满足安全单个 basename 要求。");
    }
    Ok(name.into())
}

pub(crate) enum TransferOutcome {
    Completed(TransferReceipt),
    Skipped,
    Failed {
        message: String,
        requires_review: bool,
        journal_path: Option<PathBuf>,
    },
}

pub(crate) struct TransferReport {
    pub(crate) outcome: TransferOutcome,
    pub(crate) session_usable: bool,
}

pub(crate) struct PendingTransfer {
    pub(crate) id: u64,
    pub(crate) plan: TransferPlan,
}

pub(crate) struct TransferRecord {
    pub(crate) id: u64,
    pub(crate) target: Option<String>,
    pub(crate) outcome: TransferOutcome,
}

pub(crate) struct TransferState {
    pub(crate) cache: Option<PathBuf>,
    pub(crate) pending: Option<PendingTransfer>,
    last_target: Option<String>,
    pub(crate) terminal: Option<TransferOutcome>,
    pub(crate) preflight_notice: Option<String>,
    pub(crate) recent: std::collections::VecDeque<TransferRecord>,
    pub(crate) requires_review: bool,
    pub(crate) download_policy: FileConflictPolicy,
    next_id: u64,
    exhausted: bool,
}

impl TransferState {
    pub(crate) fn new(cache: Option<PathBuf>) -> Self {
        Self {
            cache,
            pending: None,
            last_target: None,
            terminal: None,
            preflight_notice: None,
            recent: std::collections::VecDeque::new(),
            requires_review: false,
            download_policy: FileConflictPolicy::KeepBoth,
            next_id: 0,
            exhausted: false,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub(crate) fn remote_requests_allowed(&self) -> bool {
        !self.busy() && !self.requires_review && !self.exhausted
    }
    pub(crate) fn begin(&mut self, plan: TransferPlan) -> Result<u64, &'static str> {
        if self.busy() {
            return Err("已有传输处理中；不能开始新任务。");
        }
        if self.requires_review {
            return Err("RequiresReview：请核对 journal 和实际文件，明确确认后重新连接。");
        }
        let Some(id) = self.next_id.checked_add(1) else {
            self.exhausted = true;
            return Err("传输任务编号已耗尽，请重新启动应用。");
        };
        if let Some(outcome) = self.terminal.take() {
            self.recent.push_front(TransferRecord {
                id: self.next_id,
                target: self.last_target.take(),
                outcome,
            });
            self.recent.truncate(8);
        }
        self.next_id = id;
        self.preflight_notice = None;
        self.last_target = None;
        self.pending = Some(PendingTransfer { id, plan });
        Ok(id)
    }

    /// Uses a transfer ID, never the local or remote browse revision. Navigating
    /// locally must not discard a terminal transfer result.
    pub(crate) fn complete(&mut self, id: u64, outcome: TransferOutcome) -> bool {
        if self.pending.as_ref().map(|pending| pending.id) != Some(id) {
            return false;
        }
        let Some(pending) = self.pending.take() else {
            return false;
        };
        self.last_target = Some(pending.plan.target_label());
        self.requires_review = matches!(
            &outcome,
            TransferOutcome::Failed {
                requires_review: true,
                ..
            }
        );
        self.terminal = Some(outcome);
        true
    }

    pub(crate) fn reject(&mut self, message: &str) {
        if !self.busy() && !self.requires_review {
            self.preflight_notice = Some(message.into());
        }
    }

    /// Only an explicit user action calls this. Caller clears old remote refs
    /// and requires a fresh connect; this never retries or clears a journal.
    pub(crate) fn acknowledge_review(&mut self) -> bool {
        if self.busy() || !self.requires_review {
            return false;
        }
        self.requires_review = false;
        true
    }

    pub(crate) fn summary(&self) -> String {
        if self.busy() {
            return "传输处理中 · 请勿断开设备 · 无 USB Cancel · 关闭可能需要核对 journal".into();
        }
        match &self.terminal {
            Some(TransferOutcome::Completed(receipt)) => format!(
                "{}完成: {} · {} bytes",
                match receipt.direction {
                    TransferDirection::Upload => "上传",
                    TransferDirection::Download => "下载",
                },
                receipt.actual_name,
                receipt.bytes
            ),
            Some(TransferOutcome::Skipped) => {
                "Skipped / 已跳过：目标存在，未覆盖；不是 Success。".into()
            }
            Some(TransferOutcome::Failed {
                requires_review: true,
                ..
            }) if !self.requires_review => {
                "已人工确认核对 · 请手动重新连接；原任务结果仍未知，未自动重试。".into()
            }
            Some(TransferOutcome::Failed {
                message,
                requires_review: true,
                ..
            }) => format!("RequiresReview / 结果未知：{message} · 不要重试或删除文件。"),
            Some(TransferOutcome::Failed { message, .. }) => format!("未完成: {message}"),
            None => "单文件传输 · 上传/下载由实际 HELLO 能力控制 · 不递归目录，不覆盖删除".into(),
        }
    }

    pub(crate) fn target_summary(&self) -> Option<String> {
        self.pending
            .as_ref()
            .map(|pending| pending.plan.target_label())
            .or_else(|| self.last_target.clone())
    }

    pub(crate) fn published_path(&self) -> Option<PathBuf> {
        match &self.terminal {
            Some(TransferOutcome::Completed(receipt)) => {
                receipt.local_path.as_ref().map(|path| path.to_path_buf())
            }
            _ => None,
        }
    }

    pub(crate) fn journal(&self) -> Option<PathBuf> {
        match &self.terminal {
            Some(TransferOutcome::Completed(receipt)) => Some(receipt.journal_path.to_path_buf()),
            Some(TransferOutcome::Failed { journal_path, .. }) => journal_path.clone(),
            _ => None,
        }
    }
}

/// FD-relative NOFOLLOW creation of only the two application-owned directories.
/// All existing ancestors are opened without following symlinks. The worker's
/// transfer implementation must independently revalidate its stage authority.
pub(crate) fn ensure_cache(cache: &Path) -> Result<(), &'static str> {
    use rustix::fs::{self, Mode, OFlags};
    use std::path::Component;
    let components = cache.components().collect::<Vec<_>>();
    if !cache.is_absolute()
        || components.len() < 5
        || components[components.len() - 4] != Component::Normal(std::ffi::OsStr::new("Library"))
        || components[components.len() - 3] != Component::Normal(std::ffi::OsStr::new("Caches"))
        || components[components.len() - 2] != Component::Normal(std::ffi::OsStr::new("MTP-Native"))
        || components[components.len() - 1] != Component::Normal(std::ffi::OsStr::new("transfers"))
    {
        return Err("缓存必须是 HOME/Library/Caches/MTP-Native/transfers。");
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory =
        fs::open("/", flags, Mode::empty()).map_err(|_| "无法安全打开缓存根目录。")?;
    for (index, component) in components.iter().enumerate().skip(1) {
        let Component::Normal(name) = component else {
            return Err("缓存包含不安全的路径组件。");
        };
        let next = match fs::openat(&directory, *name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(error) if error == rustix::io::Errno::NOENT && index >= components.len() - 2 => {
                match fs::mkdirat(&directory, *name, Mode::from_raw_mode(0o700)) {
                    Ok(()) => (),
                    Err(error) if error == rustix::io::Errno::EXIST => (),
                    Err(_) => return Err("无法创建应用私有传输缓存目录。"),
                }
                fs::openat(&directory, *name, flags, Mode::empty())
                    .map_err(|_| "缓存被替换或不是安全普通目录。")?
            }
            Err(_) => {
                return Err("缓存祖先缺失、是符号链接或不可访问；不会 canonicalize 到其他目录。");
            }
        };
        directory = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download(destination: &str) -> TransferPlan {
        let key = match mtp_domain::DeviceKey::new("private-ui-test") {
            Ok(key) => key,
            Err(_) => panic!("invalid test key"),
        };
        TransferPlan::Download {
            source: RemoteRef::new(
                key,
                mtp_domain::SessionGeneration(1),
                mtp_domain::StorageId(1),
                mtp_domain::ObjectId(3),
            ),
            source_name: "file.txt".into(),
            source_address: String::new(),
            destination: PathBuf::from(destination),
            policy: FileConflictPolicy::KeepBoth,
        }
    }

    fn begin(state: &mut TransferState, plan: TransferPlan) -> u64 {
        match state.begin(plan) {
            Ok(id) => id,
            Err(_) => panic!("unexpected task rejection"),
        }
    }

    fn completed_state() -> TransferState {
        fn path(value: &str) -> mtp_backend_api::LocalPath {
            match mtp_backend_api::LocalPath::from_path(Path::new(value)) {
                Ok(path) => path,
                Err(_) => panic!("invalid fixture path"),
            }
        }
        let mut state = TransferState::new(None);
        let id = begin(&mut state, download("/captured"));
        assert!(state.complete(
            id,
            TransferOutcome::Completed(TransferReceipt {
                direction: TransferDirection::Download,
                actual_name: "actual.txt".into(),
                bytes: 17,
                object_id: Some(3),
                local_path: Some(path("/captured/actual.txt")),
                journal_path: path("/captured/stage/journal.json"),
            })
        ));
        state
    }

    #[test]
    fn rejected_action_and_id_exhaustion_never_erase_completed_evidence() {
        let mut state = completed_state();
        let target = state.target_summary();
        state.reject("directory upload is unsupported");
        assert_eq!(
            state.preflight_notice.as_deref(),
            Some("directory upload is unsupported")
        );
        assert!(
            matches!(&state.terminal, Some(TransferOutcome::Completed(receipt)) if receipt.actual_name == "actual.txt" && receipt.bytes == 17)
        );
        assert_eq!(
            state.published_path(),
            Some(PathBuf::from("/captured/actual.txt"))
        );
        assert_eq!(
            state.journal(),
            Some(PathBuf::from("/captured/stage/journal.json"))
        );
        assert_eq!(state.target_summary(), target);
        state.next_id = u64::MAX;
        assert!(state.begin(download("/not-started")).is_err());
        state.reject("exhausted; restart required");
        assert!(matches!(
            state.terminal,
            Some(TransferOutcome::Completed(_))
        ));
        assert_eq!(state.target_summary(), target);
        assert!(state.journal().is_some());
        assert!(state.recent.is_empty());
    }

    #[test]
    fn new_task_archives_display_only_receipt_and_captured_target() {
        let mut state = completed_state();
        let old_target = state.target_summary();
        let id = begin(&mut state, download("/next-target"));
        assert_eq!(id, 2);
        assert!(state.terminal.is_none());
        assert_eq!(state.recent.len(), 1);
        let Some(record) = state.recent.front() else {
            panic!("missing evidence record");
        };
        assert_eq!(record.id, 1);
        assert_eq!(record.target, old_target);
        assert!(
            matches!(&record.outcome, TransferOutcome::Completed(receipt) if receipt.actual_name == "actual.txt" && receipt.journal_path.to_path_buf() == Path::new("/captured/stage/journal.json"))
        );
        assert!(state.complete(id, TransferOutcome::Skipped));
        assert!(state.summary().contains("Skipped"));
        assert!(matches!(
            &state.recent[0].outcome,
            TransferOutcome::Completed(_)
        ));
    }

    #[test]
    fn explicit_ack_does_not_change_unknown_outcome_or_its_evidence() {
        let mut state = TransferState::new(None);
        let id = begin(&mut state, download("/captured"));
        assert!(state.complete(
            id,
            TransferOutcome::Failed {
                message: "unknown".into(),
                requires_review: true,
                journal_path: Some(PathBuf::from("/captured/journal.json"))
            }
        ));
        let target = state.target_summary();
        assert!(state.acknowledge_review());
        assert!(state.summary().contains("已人工确认"));
        assert!(matches!(
            &state.terminal,
            Some(TransferOutcome::Failed {
                requires_review: true,
                ..
            })
        ));
        state.reject("unrelated invalid click");
        assert_eq!(state.target_summary(), target);
        assert_eq!(
            state.journal(),
            Some(PathBuf::from("/captured/journal.json"))
        );
    }

    #[test]
    fn recent_records_are_bounded_without_any_filesystem_cleanup() {
        let mut state = completed_state();
        for _ in 0..12 {
            let id = begin(&mut state, download("/captured"));
            assert!(state.complete(id, TransferOutcome::Skipped));
        }
        assert_eq!(state.recent.len(), 8);
        assert!(matches!(state.terminal, Some(TransferOutcome::Skipped)));
    }

    #[test]
    fn task_captures_target_and_rejects_busy_requests() {
        let mut state = TransferState::new(None);
        let target = PathBuf::from("/captured/target");
        let mut browser_target = target.clone();
        let id = begin(
            &mut state,
            download(browser_target.to_str().unwrap_or("/invalid")),
        );
        browser_target = PathBuf::from("/user/new/folder");
        assert_eq!(browser_target, PathBuf::from("/user/new/folder"));
        assert_eq!(
            state.target_summary().as_deref(),
            Some("捕获下载目标: /captured/target · 源: file.txt")
        );
        assert!(state.begin(download("/wrong/target")).is_err());
        assert!(!state.remote_requests_allowed());
        assert!(!state.complete(id + 1, TransferOutcome::Skipped));
        assert!(state.busy());
        assert!(state.complete(id, TransferOutcome::Skipped));
        assert!(!state.busy());
        assert_eq!(
            state.target_summary().as_deref(),
            Some("捕获下载目标: /captured/target · 源: file.txt")
        );
        assert!(state.summary().contains("Skipped"));
        assert!(!matches!(
            state.terminal,
            Some(TransferOutcome::Completed(_))
        ));
    }

    #[test]
    fn review_barrier_survives_new_requests_until_explicit_acknowledgment() {
        let mut state = TransferState::new(None);
        let id = begin(&mut state, download("/captured"));
        assert!(!state.acknowledge_review());
        assert!(state.complete(
            id,
            TransferOutcome::Failed {
                message: "设备操作超时".into(),
                requires_review: true,
                journal_path: Some(PathBuf::from("/captured/journal.json"))
            }
        ));
        assert!(!state.remote_requests_allowed());
        assert!(state.begin(download("/retry")).is_err());
        state.reject("must not replace review evidence");
        assert_eq!(
            state.journal(),
            Some(PathBuf::from("/captured/journal.json"))
        );
        assert!(state.summary().contains("RequiresReview"));
        assert!(state.acknowledge_review());
        assert!(state.remote_requests_allowed());
        assert!(state.journal().is_some());
    }

    #[test]
    fn completed_receipt_displays_final_name_bytes_path_and_journal() {
        fn path(value: &str) -> mtp_backend_api::LocalPath {
            match mtp_backend_api::LocalPath::from_path(Path::new(value)) {
                Ok(path) => path,
                Err(_) => panic!("invalid test path"),
            }
        }
        let mut state = TransferState::new(None);
        let id = begin(&mut state, download("/captured"));
        let receipt = TransferReceipt {
            direction: TransferDirection::Download,
            actual_name: "file-mtp-0123456789abcdef0123456789abcdef.txt".into(),
            bytes: 3,
            object_id: Some(7),
            local_path: Some(path(
                "/captured/file-mtp-0123456789abcdef0123456789abcdef.txt",
            )),
            journal_path: path("/captured/stage/journal.json"),
        };
        assert!(state.complete(id, TransferOutcome::Completed(receipt)));
        assert!(state.summary().contains("3 bytes"));
        assert!(state.summary().contains("file-mtp-"));
        assert_eq!(
            state.journal(),
            Some(PathBuf::from("/captured/stage/journal.json"))
        );
        assert!(state.published_path().is_some());
        assert!(!state.requires_review);
    }

    #[test]
    fn task_ids_never_wrap_and_exhaustion_rejects_more_work() {
        let mut state = TransferState::new(None);
        state.next_id = u64::MAX;
        assert!(state.begin(download("/captured")).is_err());
        assert_eq!(state.next_id, u64::MAX);
        assert!(!state.busy());
        assert!(!state.remote_requests_allowed());
    }

    #[test]
    fn invalid_cache_shapes_are_rejected_before_any_filesystem_creation() {
        assert!(ensure_cache(Path::new("relative/Library/Caches/MTP-Native/transfers")).is_err());
        assert!(ensure_cache(Path::new("/home/test/other/transfers")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_utf8_source_basename_is_rejected_without_lossy_conversion() {
        use std::os::unix::ffi::OsStringExt;
        let name = std::ffi::OsString::from_vec(vec![b'f', 0xff]);
        assert!(upload_name(&name).is_err());
        assert_eq!(name.into_vec(), vec![b'f', 0xff]);
    }

    #[test]
    fn upload_basename_rules_do_not_silently_repair_names() {
        assert_eq!(
            upload_name(std::ffi::OsStr::new("中文.txt")),
            Ok("中文.txt".into())
        );
        assert!(upload_name(std::ffi::OsStr::new("a/b")).is_err());
        assert!(upload_name(std::ffi::OsStr::new("a\\b")).is_err());
        assert!(upload_name(std::ffi::OsStr::new("a\nb")).is_err());
    }
}
