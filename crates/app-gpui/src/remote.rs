//! Read-only worker-backed remote browsing. All worker access belongs on a background thread.

use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use mtp_application::{
    ApplicationError, BackendConfig, RemoteEntry, SessionInfo, WorkerClient, WorkerStopHandle,
};
use mtp_backend_api::Storage;
use mtp_domain::{RemoteDirectoryRef, RemoteRef, StorageId};

#[derive(Clone)]
pub(crate) enum RemoteConfig {
    LocalOnly,
    Unconfigured,
    Invalid(String),
    Ready {
        worker_path: PathBuf,
        library_path: PathBuf,
    },
}

fn parse_library(
    args: impl IntoIterator<Item = OsString>,
    env: Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut args = args.into_iter();
    let mut library = None;
    while let Some(arg) = args.next() {
        if arg != "--library" {
            return Err("未知启动参数；仅支持 --library ABSOLUTE_PATH。".into());
        }
        if library.is_some() {
            return Err("--library 只能指定一次。".into());
        }
        library = Some(
            args.next()
                .ok_or_else(|| "--library 缺少绝对路径参数。".to_owned())?,
        );
    }
    let Some(library) = library.or(env.filter(|value| !value.is_empty())) else {
        return Ok(None);
    };
    let library = PathBuf::from(library);
    if !library.is_absolute() {
        return Err("Kalam 动态库必须使用绝对路径。".into());
    }
    Ok(Some(library))
}

impl RemoteConfig {
    /// Environment, executable location and bundle file check happen at startup,
    /// never Render. Explicit configuration wins over the bundle-local resource.
    pub(crate) fn from_startup() -> Self {
        Self::from_inputs(
            std::env::args_os().skip(1),
            std::env::var_os("MTP_KALAM_LIBRARY"),
            std::env::current_exe().ok(),
            |path| std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()),
        )
    }

    fn from_inputs(
        args: impl IntoIterator<Item = OsString>,
        env: Option<OsString>,
        exe: Option<PathBuf>,
        resource_is_file: impl FnOnce(&std::path::Path) -> bool,
    ) -> Self {
        let args = args.into_iter().collect::<Vec<_>>();
        if args.iter().any(|arg| arg == "--local-only") {
            return Self::LocalOnly;
        }
        let explicit = match parse_library(args, env) {
            Ok(library) => library,
            Err(error) => return Self::Invalid(error),
        };
        let library_path = match explicit {
            Some(library) => library,
            None => {
                let Some(resource) = exe.as_deref().and_then(Self::bundle_library) else {
                    return Self::Unconfigured;
                };
                if !resource_is_file(&resource) {
                    return Self::Unconfigured;
                }
                resource
            }
        };
        match exe.as_deref().and_then(std::path::Path::parent) {
            Some(parent) => Self::Ready {
                worker_path: parent.join("mtp-worker"),
                library_path,
            },
            None => Self::Invalid("无法读取当前程序位置以定位 mtp-worker。".into()),
        }
    }

    fn bundle_library(exe: &std::path::Path) -> Option<PathBuf> {
        use std::ffi::OsStr;
        if !exe.is_absolute() || exe.file_name()? != OsStr::new("app-gpui") {
            return None;
        }
        let macos = exe.parent()?;
        let contents = macos.parent()?;
        let bundle = contents.parent()?;
        if macos.file_name()? != OsStr::new("MacOS")
            || contents.file_name()? != OsStr::new("Contents")
            || bundle.extension()? != OsStr::new("app")
        {
            return None;
        }
        Some(contents.join("Resources/kalam.dylib"))
    }

    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::LocalOnly => "仅本地交互预览 · --local-only 优先 · 不启动 worker / 不访问 USB",
            Self::Unconfigured => "尚未配置 · 使用 --library ABS 或 MTP_KALAM_LIBRARY",
            Self::Invalid(_) => "设备配置无效 · 请修正启动参数后重启",
            Self::Ready { .. } => "Kalam 已配置 · 尚未连接设备",
        }
    }
}

/// The mutex is acquired only by background jobs. The atomic also fences stale
/// operations before worker mutation, since executor scheduling is not FIFO.
pub(crate) struct SharedWorker {
    slot: Mutex<Option<WorkerClient>>,
    stop: Mutex<Option<WorkerStopHandle>>,
    closed: AtomicBool,
    desired: AtomicU64,
}

impl SharedWorker {
    fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            stop: Mutex::new(None),
            closed: AtomicBool::new(false),
            desired: AtomicU64::new(0),
        }
    }

    /// Quick process kill only: no business mutex, no wait, no USB Cancel claim.
    pub(crate) fn request_stop(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.desired.store(0, Ordering::SeqCst);
        let stop = self
            .stop
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(stop) = stop.as_ref() {
            let _ = stop.request_stop();
        }
    }

    fn register_stop(&self, handle: WorkerStopHandle) -> bool {
        let mut stop = self
            .stop
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.closed.load(Ordering::SeqCst) {
            let _ = handle.request_stop();
            return false;
        }
        *stop = Some(handle);
        true
    }
}

impl Drop for SharedWorker {
    fn drop(&mut self) {
        self.request_stop();
        // WorkerClient::drop kills/waits. Never perform that blocking cleanup on
        // the UI thread when a window releases the final shared handle.
        let slot = self
            .slot
            .get_mut()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(client) = slot.take() {
            std::thread::spawn(move || drop(client));
        }
    }
}

#[derive(Clone)]
pub(crate) struct RemoteLocation {
    pub(crate) directory: RemoteDirectoryRef,
    pub(crate) label: String,
}

pub(crate) enum RemoteCommand {
    Connect(RemoteConfig),
    Disconnect,
    List(RemoteDirectoryRef),
}

pub(crate) enum RemoteReply {
    Connected {
        session: SessionInfo,
        storages: Vec<Storage>,
        root: Option<RemoteLocation>,
        entries: Vec<RemoteEntry>,
    },
    Listed(Vec<RemoteEntry>),
    Disconnected,
}

pub(crate) struct RemoteBrowser {
    pub(crate) config: RemoteConfig,
    pub(crate) shared: Arc<SharedWorker>,
    pub(crate) session: Option<SessionInfo>,
    pub(crate) storages: Vec<Storage>,
    pub(crate) locations: Vec<RemoteLocation>,
    pub(crate) entries: Vec<RemoteEntry>,
    pub(crate) selected: Option<RemoteRef>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) exhausted: bool,
    revision: u64,
}

impl RemoteBrowser {
    pub(crate) fn new(config: RemoteConfig) -> Self {
        let error = match &config {
            RemoteConfig::Invalid(error) => Some(error.clone()),
            _ => None,
        };
        Self {
            config,
            shared: Arc::new(SharedWorker::new()),
            session: None,
            storages: Vec::new(),
            locations: Vec::new(),
            entries: Vec::new(),
            selected: None,
            loading: false,
            error,
            exhausted: false,
            revision: 0,
        }
    }

    pub(crate) fn clear_references(&mut self) {
        self.session = None;
        self.storages.clear();
        self.locations.clear();
        self.entries.clear();
        self.selected = None;
    }

    pub(crate) fn begin(&mut self, command: &RemoteCommand) -> Option<u64> {
        if matches!(self.config, RemoteConfig::LocalOnly) {
            return None;
        }
        if self.shared.closed.load(Ordering::SeqCst) {
            return None;
        }
        let Some(revision) = self.revision.checked_add(1) else {
            self.exhausted = true;
            self.shared.desired.store(0, Ordering::SeqCst);
            self.clear_references();
            self.loading = false;
            self.error = Some("设备请求编号已耗尽，请重新启动应用。".into());
            return None;
        };
        self.revision = revision;
        self.shared.desired.store(revision, Ordering::SeqCst);
        if matches!(
            command,
            RemoteCommand::Connect(_) | RemoteCommand::Disconnect
        ) {
            self.clear_references();
        } else {
            self.entries.clear();
            self.selected = None;
        }
        self.error = None;
        self.loading = true;
        Some(revision)
    }

    pub(crate) fn complete(&mut self, revision: u64, result: Result<RemoteReply, String>) -> bool {
        if self.exhausted || revision != self.revision {
            return false;
        }
        self.loading = false;
        match result {
            Ok(RemoteReply::Connected {
                session,
                storages,
                root,
                entries,
            }) => {
                self.session = Some(session);
                self.storages = storages;
                self.locations = root.into_iter().collect();
                self.entries = entries;
            }
            Ok(RemoteReply::Listed(entries)) => self.entries = entries,
            Ok(RemoteReply::Disconnected) => self.clear_references(),
            Err(error) => {
                self.clear_references();
                self.error = Some(error);
            }
        }
        true
    }

    pub(crate) fn transfer_epoch(&self) -> u64 {
        self.revision
    }

    pub(crate) fn current(&self) -> Option<&RemoteDirectoryRef> {
        self.locations.last().map(|location| &location.directory)
    }

    pub(crate) fn root_location(&self, index: usize) -> Option<RemoteLocation> {
        let storage = self.storages.get(index)?;
        let session = self.session.as_ref()?;
        Some(RemoteLocation {
            directory: session.root(StorageId(storage.id)),
            label: storage.name.clone(),
        })
    }

    pub(crate) fn address(&self) -> String {
        if self.locations.is_empty() {
            return "未选择存储 / No storage".into();
        }
        format!(
            "/{}",
            self.locations
                .iter()
                .map(|location| location.label.as_str())
                .collect::<Vec<_>>()
                .join("/")
        )
    }
}

/// Never format raw backend errors, device keys, sessions, or remote references:
/// backend messages may contain identifiers that must not reach UI/logs.
fn safe_error(error: ApplicationError) -> String {
    match error {
        ApplicationError::Backend { code, .. } => {
            format!("设备操作失败 ({code:?})；连接已清除，请重连。")
        }
        ApplicationError::Io(_) => "worker IO 失败；请检查程序和动态库路径及设备状态。".into(),
        ApplicationError::Protocol(_)
        | ApplicationError::UnexpectedResponse
        | ApplicationError::IncompatibleHello => "worker 协议或握手无效；连接已清除。".into(),
        ApplicationError::TimedOut(_) => "设备操作超时；连接已清除，请重连。".into(),
        ApplicationError::Disconnected | ApplicationError::NotConnected => {
            "设备已断开；请重新连接。".into()
        }
        ApplicationError::Domain(_) => "远端引用或名称无效；连接已清除，请重连。".into(),
        ApplicationError::UnknownOutcome { .. } => {
            "设备操作结果未知；需要核对 journal 和实际文件。".into()
        }
    }
}

fn sorted(mut entries: Vec<RemoteEntry>) -> Vec<RemoteEntry> {
    entries.sort_by(|a, b| (!a.directory, &a.name).cmp(&(!b.directory, &b.name)));
    entries
}

/// Blocking function: the caller must execute this on GPUI's background executor.
/// None means an obsolete operation was skipped, not a successful device action.
pub(crate) fn run(
    shared: Arc<SharedWorker>,
    revision: u64,
    command: RemoteCommand,
) -> Option<Result<RemoteReply, String>> {
    let mut slot = match shared.slot.lock() {
        Ok(slot) => slot,
        Err(_) => return Some(Err("worker 状态锁不可用；请重启应用。".into())),
    };
    if shared.desired.load(Ordering::SeqCst) != revision {
        return None;
    }
    let result: Result<Option<RemoteReply>, String> = match command {
        RemoteCommand::Connect(RemoteConfig::Ready {
            worker_path,
            library_path,
        }) => {
            // Restart always replaces an old client, even if its disconnect was
            // overtaken by this newer task. Drop remains on this background thread.
            if let Some(mut old) = slot.take() {
                let _ = old.disconnect();
            }
            (|| {
                if shared.desired.load(Ordering::SeqCst) != revision {
                    return Ok(None);
                }
                let mut client =
                    WorkerClient::spawn(&worker_path, &BackendConfig::Kalam { library_path })
                        .map_err(safe_error)?;
                if !shared.register_stop(client.stop_handle()) {
                    return Ok(None);
                }
                if shared.desired.load(Ordering::SeqCst) != revision {
                    return Ok(None);
                }
                let session = client.connect().map_err(safe_error)?;
                if shared.desired.load(Ordering::SeqCst) != revision {
                    return Ok(None);
                }
                if !session.capabilities.browse {
                    return Err("设备不支持目录浏览。".into());
                }
                let storages = client.storages().map_err(safe_error)?;
                if shared.desired.load(Ordering::SeqCst) != revision {
                    return Ok(None);
                }
                let root = storages.first().map(|storage| RemoteLocation {
                    directory: session.root(StorageId(storage.id)),
                    label: storage.name.clone(),
                });
                let entries = match &root {
                    Some(root) => sorted(client.list(&root.directory).map_err(safe_error)?),
                    None => Vec::new(),
                };
                if shared.desired.load(Ordering::SeqCst) != revision {
                    return Ok(None);
                }
                *slot = Some(client);
                Ok(Some(RemoteReply::Connected {
                    session,
                    storages,
                    root,
                    entries,
                }))
            })()
        }
        RemoteCommand::Connect(_) => Err("尚未配置有效 Kalam 动态库。".into()),
        RemoteCommand::Disconnect => {
            let result = match slot.take() {
                Some(mut client) => client.disconnect().map_err(safe_error),
                None => Ok(()),
            };
            result.map(|()| Some(RemoteReply::Disconnected))
        }
        RemoteCommand::List(directory) => match slot.as_mut() {
            Some(client) => client
                .list(&directory)
                .map(|entries| Some(RemoteReply::Listed(sorted(entries))))
                .map_err(safe_error),
            None => Err("设备已断开；请重新连接。".into()),
        },
    };
    if result.is_err() {
        drop(slot.take());
    }
    if shared.desired.load(Ordering::SeqCst) != revision {
        return None;
    }
    match result {
        Ok(Some(reply)) => Some(Ok(reply)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

fn transfer_error(error: ApplicationError) -> crate::transfers::TransferOutcome {
    use crate::transfers::TransferOutcome;
    use mtp_backend_api::ErrorCode;
    match error {
        ApplicationError::UnknownOutcome { journal_path } => TransferOutcome::Failed {
            message: "设备未提供可确认的传输终态".into(),
            requires_review: true,
            journal_path: journal_path.map(|path| path.to_path_buf()),
        },
        ApplicationError::Backend {
            code: ErrorCode::Skipped,
            requires_review: false,
            ..
        } => TransferOutcome::Skipped,
        ApplicationError::Backend {
            code,
            requires_review,
            ..
        } => TransferOutcome::Failed {
            message: format!("设备操作失败 ({code:?})"),
            requires_review: requires_review || code == ErrorCode::UnknownOutcome,
            journal_path: None,
        },
        ApplicationError::TimedOut(_)
        | ApplicationError::Disconnected
        | ApplicationError::Protocol(_)
        | ApplicationError::UnexpectedResponse
        | ApplicationError::IncompatibleHello => TransferOutcome::Failed {
            message: "设备连接中断、超时或返回无效；不能确认结果".into(),
            requires_review: true,
            journal_path: None,
        },
        ApplicationError::NotConnected => TransferOutcome::Failed {
            message: "设备未连接，未发起传输".into(),
            requires_review: false,
            journal_path: None,
        },
        ApplicationError::Io(_) => TransferOutcome::Failed {
            message: "本地预检查或 worker IO 失败".into(),
            requires_review: false,
            journal_path: None,
        },
        ApplicationError::Domain(_) => TransferOutcome::Failed {
            message: "文件名或远端会话引用无效".into(),
            requires_review: false,
            journal_path: None,
        },
    }
}

/// Always returns an independent terminal report, not an optional browse result.
/// The UI freezes remote navigation/epoch while this task owns the client.
pub(crate) fn run_transfer(
    shared: Arc<SharedWorker>,
    epoch: u64,
    plan: crate::transfers::TransferPlan,
) -> crate::transfers::TransferReport {
    use crate::transfers::{TransferOutcome, TransferPlan, TransferReport};
    let mut slot = match shared.slot.lock() {
        Ok(slot) => slot,
        Err(_) => {
            return TransferReport {
                outcome: TransferOutcome::Failed {
                    message: "worker 状态锁异常；需要核对结果并重启".into(),
                    requires_review: true,
                    journal_path: None,
                },
                session_usable: false,
            };
        }
    };
    if shared.closed.load(Ordering::SeqCst) || shared.desired.load(Ordering::SeqCst) != epoch {
        return TransferReport {
            outcome: TransferOutcome::Failed {
                message: "捕获会话已经失效，未发起传输".into(),
                requires_review: false,
                journal_path: None,
            },
            session_usable: false,
        };
    }
    let Some(client) = slot.as_mut() else {
        return TransferReport {
            outcome: transfer_error(ApplicationError::NotConnected),
            session_usable: false,
        };
    };
    let result = match plan {
        TransferPlan::Upload {
            source,
            requested_name,
            destination,
            cache,
            ..
        } => {
            if let Err(message) = crate::transfers::ensure_cache(&cache) {
                return TransferReport {
                    outcome: TransferOutcome::Failed {
                        message: message.into(),
                        requires_review: false,
                        journal_path: None,
                    },
                    session_usable: client.session().is_some(),
                };
            }
            client.upload_file(&source, &requested_name, &destination, &cache)
        }
        TransferPlan::Download {
            source,
            destination,
            policy,
            ..
        } => client.download_file(&source, &destination, policy),
    };
    let outcome = match result {
        Ok(receipt) => TransferOutcome::Completed(receipt),
        Err(error) => transfer_error(error),
    };
    let review = matches!(
        &outcome,
        TransferOutcome::Failed {
            requires_review: true,
            ..
        }
    );
    let mut session_usable = client.session().is_some();
    if review || !session_usable {
        drop(slot.take());
        session_usable = false;
    }
    TransferReport {
        outcome,
        session_usable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_overrides_all_configuration_without_resource_probe() {
        let config = RemoteConfig::from_inputs(
            [
                OsString::from("--library"),
                OsString::from("/must-not-load.dylib"),
                OsString::from("--local-only"),
            ],
            Some(OsString::from("/also-must-not-load.dylib")),
            Some(PathBuf::from("/bundles/Test.app/Contents/MacOS/app-gpui")),
            |_| panic!("local-only must not probe bundle resources"),
        );
        assert!(matches!(config, RemoteConfig::LocalOnly));
        let malformed = RemoteConfig::from_inputs(
            [OsString::from("--local-only"), OsString::from("--library")],
            None,
            None,
            |_| panic!("local-only must not probe resources"),
        );
        assert!(matches!(malformed, RemoteConfig::LocalOnly));
        assert!(config.message().contains("不启动 worker"));
    }

    #[test]
    fn local_only_refuses_every_epoch_before_any_worker_dispatch() {
        let mut browser = RemoteBrowser::new(RemoteConfig::LocalOnly);
        let configured = RemoteCommand::Connect(RemoteConfig::Ready {
            worker_path: PathBuf::from("/must-not-execute"),
            library_path: PathBuf::from("/must-not-load.dylib"),
        });
        assert!(browser.begin(&configured).is_none());
        assert!(browser.begin(&RemoteCommand::Disconnect).is_none());
        assert_eq!(browser.transfer_epoch(), 0);
        assert!(!browser.loading);
        assert!(browser.session.is_none());
        assert_eq!(browser.shared.desired.load(Ordering::SeqCst), 0);
        let slot = match browser.shared.slot.lock() {
            Ok(slot) => slot,
            Err(_) => panic!("poisoned fixture lock"),
        };
        assert!(slot.is_none());
    }

    #[test]
    fn process_stop_does_not_need_the_busy_worker_slot() {
        let mut browser = RemoteBrowser::new(RemoteConfig::Unconfigured);
        let _ = begin(
            &mut browser,
            &RemoteCommand::Connect(RemoteConfig::Unconfigured),
        );
        let shared = browser.shared.clone();
        let _business_guard = match shared.slot.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("poisoned test lock"),
        };
        shared.request_stop();
        assert!(shared.closed.load(Ordering::SeqCst));
        assert_eq!(shared.desired.load(Ordering::SeqCst), 0);
        assert!(browser.begin(&RemoteCommand::Disconnect).is_none());
    }

    #[test]
    fn transfer_skip_is_not_success_and_review_evidence_is_preserved() {
        use crate::transfers::TransferOutcome;
        let skipped = transfer_error(ApplicationError::Backend {
            code: mtp_backend_api::ErrorCode::Skipped,
            message: "private backend details".into(),
            requires_review: false,
        });
        assert!(matches!(skipped, TransferOutcome::Skipped));
        let journal = match mtp_backend_api::LocalPath::from_path(std::path::Path::new(
            "/safe/cache/journal.json",
        )) {
            Ok(path) => path,
            Err(_) => panic!("invalid test journal path"),
        };
        let unknown = transfer_error(ApplicationError::UnknownOutcome {
            journal_path: Some(journal),
        });
        assert!(matches!(
            unknown,
            TransferOutcome::Failed {
                requires_review: true,
                journal_path: Some(_),
                ..
            }
        ));
        let timeout = transfer_error(ApplicationError::TimedOut(std::time::Duration::from_secs(
            1,
        )));
        assert!(matches!(
            timeout,
            TransferOutcome::Failed {
                requires_review: true,
                ..
            }
        ));
        let backend_review = transfer_error(ApplicationError::Backend {
            code: mtp_backend_api::ErrorCode::Skipped,
            message: "private backend details".into(),
            requires_review: true,
        });
        assert!(matches!(
            backend_review,
            TransferOutcome::Failed {
                requires_review: true,
                ..
            }
        ));
    }

    #[test]
    fn explicit_library_wins_and_relative_paths_are_rejected() {
        assert_eq!(
            parse_library(
                [
                    OsString::from("--library"),
                    OsString::from("/explicit.dylib")
                ],
                Some(OsString::from("/env.dylib"))
            ),
            Ok(Some(PathBuf::from("/explicit.dylib")))
        );
        assert!(
            parse_library(
                [
                    OsString::from("--library"),
                    OsString::from("relative.dylib")
                ],
                None
            )
            .is_err()
        );
        assert!(parse_library([OsString::from("--library")], None).is_err());
        assert_eq!(parse_library([], None), Ok(None));
    }

    #[test]
    fn startup_explicit_library_wins_without_probing_bundle() {
        let config = RemoteConfig::from_inputs(
            [
                OsString::from("--library"),
                OsString::from("/explicit/kalam.dylib"),
            ],
            Some(OsString::from("/env/kalam.dylib")),
            Some(PathBuf::from("/bundles/Test.app/Contents/MacOS/app-gpui")),
            |_| panic!("explicit configuration must not probe resources"),
        );
        match config {
            RemoteConfig::Ready {
                worker_path,
                library_path,
            } => {
                assert_eq!(library_path, PathBuf::from("/explicit/kalam.dylib"));
                assert_eq!(
                    worker_path,
                    PathBuf::from("/bundles/Test.app/Contents/MacOS/mtp-worker")
                );
            }
            _ => panic!("expected explicit startup config"),
        }
        let env_config = RemoteConfig::from_inputs(
            [],
            Some(OsString::from("/env/kalam.dylib")),
            Some(PathBuf::from("/target/debug/app-gpui")),
            |_| panic!("environment must not probe resources"),
        );
        match env_config {
            RemoteConfig::Ready { library_path, .. } => {
                assert_eq!(library_path, PathBuf::from("/env/kalam.dylib"))
            }
            _ => panic!("expected environment startup config"),
        }
    }

    #[test]
    fn startup_finder_uses_only_its_own_existing_resource() {
        let config = RemoteConfig::from_inputs(
            [],
            None,
            Some(PathBuf::from("/bundles/Test.app/Contents/MacOS/app-gpui")),
            |candidate| {
                assert_eq!(
                    candidate,
                    std::path::Path::new("/bundles/Test.app/Contents/Resources/kalam.dylib")
                );
                true
            },
        );
        match config {
            RemoteConfig::Ready {
                worker_path,
                library_path,
            } => {
                assert_eq!(
                    worker_path,
                    PathBuf::from("/bundles/Test.app/Contents/MacOS/mtp-worker")
                );
                assert_eq!(
                    library_path,
                    PathBuf::from("/bundles/Test.app/Contents/Resources/kalam.dylib")
                );
            }
            _ => panic!("expected bundle-local startup config"),
        }
    }

    #[test]
    fn startup_missing_resource_or_bare_binary_has_no_fallback() {
        let missing = RemoteConfig::from_inputs(
            [],
            None,
            Some(PathBuf::from("/bundles/Test.app/Contents/MacOS/app-gpui")),
            |_| false,
        );
        assert!(matches!(missing, RemoteConfig::Unconfigured));
        let bare = RemoteConfig::from_inputs(
            [],
            None,
            Some(PathBuf::from("/target/debug/app-gpui")),
            |_| panic!("bare binary must not search other resource directories"),
        );
        assert!(matches!(bare, RemoteConfig::Unconfigured));
        let unknown = RemoteConfig::from_inputs([], None, None, |_| {
            panic!("no executable means no resource lookup")
        });
        assert!(matches!(unknown, RemoteConfig::Unconfigured));
    }

    #[test]
    fn startup_invalid_explicit_config_never_falls_back_to_bundle() {
        let invalid = RemoteConfig::from_inputs(
            [
                OsString::from("--library"),
                OsString::from("relative.dylib"),
            ],
            None,
            Some(PathBuf::from("/bundles/Test.app/Contents/MacOS/app-gpui")),
            |_| panic!("invalid explicit config must not fall back"),
        );
        assert!(matches!(invalid, RemoteConfig::Invalid(_)));
    }

    fn connected_reply() -> RemoteReply {
        let device_key = match mtp_domain::DeviceKey::new("private-test-identifier") {
            Ok(key) => key,
            Err(_) => panic!("invalid test key"),
        };
        let session = SessionInfo {
            device_key,
            display_name: "Test display name".into(),
            generation: mtp_domain::SessionGeneration(123),
            capabilities: mtp_backend_api::Capabilities {
                simulated: false,
                browse: true,
                upload: false,
                download: false,
                cancel_current: false,
            },
        };
        let storage = Storage {
            id: 1,
            name: "Storage".into(),
            capacity: None,
        };
        let root = RemoteLocation {
            directory: session.root(StorageId(storage.id)),
            label: storage.name.clone(),
        };
        let entry = RemoteEntry {
            reference: RemoteRef::new(
                session.device_key.clone(),
                session.generation,
                StorageId(storage.id),
                mtp_domain::ObjectId(7),
            ),
            name: "Folder".into(),
            directory: true,
            byte_size: None,
        };
        RemoteReply::Connected {
            session,
            storages: vec![storage],
            root: Some(root),
            entries: vec![entry],
        }
    }

    fn begin(browser: &mut RemoteBrowser, command: &RemoteCommand) -> u64 {
        match browser.begin(command) {
            Some(revision) => revision,
            None => panic!("unexpected revision exhaustion"),
        }
    }

    #[test]
    fn disconnect_fences_late_results_and_clears_navigation() {
        let mut browser = RemoteBrowser::new(RemoteConfig::Unconfigured);
        let connected = begin(
            &mut browser,
            &RemoteCommand::Connect(RemoteConfig::Unconfigured),
        );
        assert!(browser.complete(connected, Ok(connected_reply())));
        browser.selected = browser.entries.first().map(|entry| entry.reference.clone());
        assert!(browser.session.is_some());
        assert!(!browser.storages.is_empty());
        assert!(!browser.locations.is_empty());
        assert!(!browser.entries.is_empty());
        assert!(browser.selected.is_some());
        let Some(directory) = browser.current().cloned() else {
            panic!("missing test root");
        };
        let old = begin(&mut browser, &RemoteCommand::List(directory));
        let current = begin(&mut browser, &RemoteCommand::Disconnect);
        assert!(browser.session.is_none());
        assert!(browser.storages.is_empty());
        assert!(browser.locations.is_empty());
        assert!(browser.entries.is_empty());
        assert!(browser.selected.is_none());
        assert!(!browser.complete(connected, Ok(connected_reply())));
        assert!(!browser.complete(old, Ok(RemoteReply::Listed(Vec::new()))));
        assert!(!browser.complete(old, Err("obsolete list".into())));
        assert!(browser.session.is_none());
        assert!(browser.loading);
        assert!(browser.complete(current, Ok(RemoteReply::Disconnected)));
        assert!(browser.error.is_none());
        assert!(!browser.loading);
        assert!(run(browser.shared.clone(), old, RemoteCommand::Disconnect).is_none());
    }

    #[test]
    fn shared_epoch_skips_a_connect_queued_before_disconnect() {
        let mut browser = RemoteBrowser::new(RemoteConfig::Unconfigured);
        let command = RemoteCommand::Connect(RemoteConfig::Ready {
            worker_path: PathBuf::from("/must-not-spawn-mtp-worker"),
            library_path: PathBuf::from("/must-not-load.dylib"),
        });
        let old = begin(&mut browser, &command);
        let shared = browser.shared.clone();
        assert!(Arc::ptr_eq(&shared, &browser.shared));
        let guard = match shared.slot.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("poisoned test lock"),
        };
        let (started, receiver) = std::sync::mpsc::channel();
        let task_shared = shared.clone();
        let task = std::thread::spawn(move || {
            assert!(started.send(()).is_ok());
            run(task_shared, old, command)
        });
        assert!(receiver.recv().is_ok());
        let current = begin(&mut browser, &RemoteCommand::Disconnect);
        assert_eq!(shared.desired.load(Ordering::SeqCst), current);
        drop(guard);
        let skipped = match task.join() {
            Ok(result) => result.is_none(),
            Err(_) => panic!("test task panicked"),
        };
        assert!(skipped);
        assert!(browser.complete(current, Ok(RemoteReply::Disconnected)));
        assert!(browser.session.is_none());
    }

    #[test]
    fn backend_messages_never_reach_display_errors() {
        let message = safe_error(ApplicationError::Backend {
            code: mtp_backend_api::ErrorCode::BackendFailure,
            message: "private-test-identifier serial-secret".into(),
            requires_review: false,
        });
        assert!(!message.contains("private-test-identifier"));
        assert!(!message.contains("serial-secret"));
        assert!(message.contains("BackendFailure"));
    }

    #[test]
    fn remote_revision_exhaustion_does_not_wrap() {
        let mut browser = RemoteBrowser::new(RemoteConfig::Unconfigured);
        browser.revision = u64::MAX;
        assert!(browser.begin(&RemoteCommand::Disconnect).is_none());
        assert_eq!(browser.revision, u64::MAX);
        assert_eq!(browser.shared.desired.load(Ordering::SeqCst), 0);
        assert!(browser.exhausted);
        assert!(!browser.complete(u64::MAX, Ok(RemoteReply::Disconnected)));
        assert!(browser.error.is_some());
    }
}
