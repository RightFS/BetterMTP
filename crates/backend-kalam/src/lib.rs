//! Narrow Kalam C ABI adapter: read-only compatibility and trusted v2 FD transfers.
//! Loading native code is a trust boundary: only use an explicitly selected,
//! vetted 64-bit library. Native crashes/hangs are not catchable Rust errors.
#![deny(unsafe_op_in_unsafe_fn)]

use libloading::Library;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{CString, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

mod inspect;
pub use inspect::InspectUploadDestinationRequest;
mod transfer;
pub use transfer::{SafeDownloadRequest, SafeUploadPolicy, SafeUploadRequest};

pub const MAX_CALLBACK_BYTES: usize = 1024 * 1024;
pub type Result<T> = std::result::Result<T, KalamError>;

#[derive(Debug, thiserror::Error)]
pub enum KalamError {
    #[error("Kalam requires an explicit absolute library path")]
    LibraryPath,
    #[error("Kalam ABI requires a 64-bit Unix worker")]
    UnsupportedPlatform,
    #[error("this worker process has already loaded a Kalam library; start a fresh worker")]
    OwnerAlreadyClaimed,
    #[error("cannot load Kalam library {path}: {source}")]
    Load {
        path: PathBuf,
        source: libloading::Error,
    },
    #[error("required Kalam symbol {symbol} is missing: {source}")]
    MissingSymbol {
        symbol: &'static str,
        source: libloading::Error,
    },
    #[error("Kalam operation {operation} is invalid in state {state}")]
    State {
        operation: &'static str,
        state: &'static str,
    },
    #[error("invalid Kalam input: {0}")]
    Input(String),
    #[error("Kalam did not invoke its terminal callback")]
    MissingCallback,
    #[error("Kalam invoked its terminal callback more than once")]
    DuplicateCallback,
    #[error("Kalam callback payload was null")]
    NullCallback,
    #[error("Kalam callback JSON exceeds the 1 MiB limit")]
    CallbackTooLarge,
    #[error("Kalam callback panicked internally (unwind was contained)")]
    CallbackPanic,
    #[error("Kalam callback arrived outside a synchronous operation")]
    UnexpectedCallback,
    #[error("invalid Kalam JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid Kalam result envelope: {0}")]
    Envelope(String),
    #[error(
        "Kalam FD transfer {operation} is unavailable; verified SafetyVersion 2 and its safe symbol are required"
    )]
    TransferUnavailable { operation: &'static str },
    #[error(
        "Kalam read-only destination inspection is unavailable; verified SafetyVersion 2 and its inspection symbol are required"
    )]
    InspectionUnavailable,
    #[error("Kalam native error (details withheld)")]
    Native {
        error_type: String,
        message: String,
        data: Option<Value>,
    },
}

/// Duplicate the protocol stdout handle, then route native stdout to stderr.
/// Call once, before loading any native library or starting other threads.
/// The returned File owns only the duplicate; stdout (fd 1) remains open.
#[cfg(unix)]
pub fn isolate_native_stdout() -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: dup/dup2 operate on the standard Unix fd numbers. The newly
    // owned duplicate is closed by File even if dup2 fails; fd 1 is never closed.
    let duplicate = unsafe { dup(1) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful dup returned a new, uniquely owned descriptor.
    let protocol = unsafe { std::fs::File::from_raw_fd(duplicate) };
    if duplicate == 2 {
        // dup filled the previously closed stderr slot; that descriptor now
        // belongs to protocol, not to a usable native diagnostic sink.
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stderr must be open before stdout isolation",
        ));
    }
    if unsafe { dup2(2, 1) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(protocol)
}

#[cfg(not(unix))]
pub fn isolate_native_stdout() -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Kalam requires Unix fd isolation",
    ))
}

unsafe extern "C" {
    fn free(ptr: *mut c_void);
    #[cfg(unix)]
    fn dup(fd: c_int) -> c_int;
    #[cfg(unix)]
    fn dup2(old: c_int, new: c_int) -> c_int;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalkRequest {
    pub storage_id: u32,
    pub full_path: String,
    pub recursive: bool,
    pub skip_disallowed_files: bool,
    pub skip_hidden_files: bool,
}

impl WalkRequest {
    pub fn directory(storage_id: u32, full_path: impl Into<String>) -> Self {
        Self {
            storage_id,
            full_path: full_path.into(),
            recursive: false,
            skip_disallowed_files: true,
            skip_hidden_files: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExistsRequest {
    #[serde(rename = "storageId")]
    pub storage_id: u32,
    // Match the actual Go struct tag, rather than guessing from JS spelling.
    #[serde(rename = "Files", alias = "files")]
    pub files: Vec<String>,
}

/// Typed legacy directory data; raw envelopes remain the primary worker API.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KEntry {
    pub size: i64,
    pub is_folder: bool,
    pub date_added: String,
    pub name: String,
    pub path: String,
    pub parent_path: String,
    pub extension: String,
    pub parent_id: u32,
    pub object_id: u32,
}

/// Device vendor fields intentionally remain unnormalized, as emitted by Go.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KDevice {
    pub mtp_device_info: Value,
    pub usb_device_info: Value,
}

/// Legacy planning data only; not accepted by the gated FD transfer methods.
/// Construction does NOT establish destination safety or authorize a write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SingleFileTransferPlan {
    pub storage_id: u32,
    pub source: String,
    pub destination_directory: String,
    pub direction: TransferDirection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

// Generated on_cb_result_t* is a disguised function ADDRESS, NOT a pointer
// to a function pointer slot. The old C shim casts this address back to a cb.
type CallbackAddress = *mut c_void;
type NoInput = unsafe extern "C" fn(CallbackAddress);
type WithInput = unsafe extern "C" fn(*mut c_char, CallbackAddress);
type SafetyVersion = unsafe extern "C" fn() -> u32;
type SafeUpload =
    unsafe extern "C" fn(*mut c_char, c_int, CallbackAddress, CallbackAddress, CallbackAddress);
type SafeDownload = unsafe extern "C" fn(*mut c_char, c_int, CallbackAddress, CallbackAddress);

struct Symbols {
    initialize: NoInput,
    device_info: NoInput,
    storages: NoInput,
    walk: WithInput,
    exists: WithInput,
    dispose: NoInput,
    safety_version: Option<u32>,
    safe_upload: Option<SafeUpload>,
    safe_download: Option<SafeDownload>,
    inspect_upload_destination: Option<WithInput>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Loaded,
    Ready,
    Failed,
    Closed,
}
impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Ready => "ready",
            Self::Failed => "failed; dispose or terminate worker",
            Self::Closed => "closed",
        }
    }
}

/// One serial owner, deliberately !Send + !Sync. A loaded library and its Go
/// runtime are pinned until process exit, including after Dispose/load errors.
/// No second instance may load in this process, even after this owner drops.
pub struct Kalam {
    _library: ManuallyDrop<Library>,
    symbols: Symbols,
    state: State,
    initialize_attempted: bool,
    _single_thread: PhantomData<Rc<()>>,
}

static OWNER: Mutex<bool> = Mutex::new(false);
static CALLBACK: Mutex<CallbackState> = Mutex::new(CallbackState {
    active: false,
    count: 0,
    result: None,
    unexpected: false,
});

static EVENT_CALLBACK: Mutex<EventCallbackState> = Mutex::new(EventCallbackState {
    active: false,
    error: None,
});

struct EventCallbackState {
    active: bool,
    error: Option<KalamError>,
}

struct CallbackState {
    active: bool,
    count: usize,
    result: Option<Result<Value>>,
    unexpected: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A contained callback panic must not cause a second panic at the boundary.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Kalam {
    /// Load exactly this library; never guess platform variants or search paths.
    /// Native library constructors run during loading, so use only trusted input.
    pub fn load(path: &Path) -> Result<Self> {
        if !cfg!(all(unix, target_pointer_width = "64")) {
            return Err(KalamError::UnsupportedPlatform);
        }
        if !path.is_absolute() {
            return Err(KalamError::LibraryPath);
        }
        let mut owner = lock(&OWNER);
        if *owner {
            return Err(KalamError::OwnerAlreadyClaimed);
        }
        // SAFETY: explicit trusted native input, worker isolation required.
        let library = unsafe { Library::new(path) }.map_err(|source| KalamError::Load {
            path: path.to_owned(),
            source,
        })?;
        *owner = true;
        // Pin immediately: even symbol-resolution failures must not unload Go.
        let library = ManuallyDrop::new(library);
        // SAFETY: these signatures exactly match the inspected header + C shim.
        // Function pointers stay valid because the library is never unloaded.
        // SAFETY: caller must select a known trusted source/artifact; an export
        // named SafetyVersion is not itself authentication of arbitrary code.
        // Only exactly 2 permits resolving the FD signatures: v1 same-name
        // transfer exports have a different ABI and must never be invoked.
        let safety_version = unsafe { library.get::<SafetyVersion>(b"SafetyVersion\0") }
            .ok()
            .map(|marker| unsafe { marker() });
        let symbols = unsafe {
            Symbols {
                initialize: symbol(&library, b"Initialize\0", "Initialize")?,
                device_info: symbol(&library, b"FetchDeviceInfo\0", "FetchDeviceInfo")?,
                storages: symbol(&library, b"FetchStorages\0", "FetchStorages")?,
                walk: symbol(&library, b"Walk\0", "Walk")?,
                exists: symbol(&library, b"FileExists\0", "FileExists")?,
                dispose: symbol(&library, b"Dispose\0", "Dispose")?,
                safety_version,
                safe_upload: if safety_version == Some(2) {
                    library
                        .get::<SafeUpload>(b"SafeUploadSingle\0")
                        .ok()
                        .map(|symbol| *symbol)
                } else {
                    None
                },
                safe_download: if safety_version == Some(2) {
                    library
                        .get::<SafeDownload>(b"SafeDownloadSingle\0")
                        .ok()
                        .map(|symbol| *symbol)
                } else {
                    None
                },
                inspect_upload_destination: if safety_version == Some(2) {
                    library
                        .get::<WithInput>(b"InspectUploadDestination\0")
                        .ok()
                        .map(|symbol| *symbol)
                } else {
                    None
                },
            }
        };
        Ok(Self {
            _library: library,
            symbols,
            state: State::Loaded,
            initialize_attempted: false,
            _single_thread: PhantomData,
        })
    }

    pub fn initialize(&mut self) -> Result<Value> {
        self.require("Initialize", State::Loaded)?;
        self.initialize_attempted = true;
        // Failed initialization may already have allocated native resources.
        self.state = State::Failed;
        let result = self.call_no_input(self.symbols.initialize);
        if result.is_ok() {
            self.state = State::Ready;
        }
        result
            .and_then(|value| validate_data(value, DataShape::Device))
            .inspect_err(|_| self.state = State::Failed)
    }

    pub fn device_info(&mut self) -> Result<Value> {
        self.require("FetchDeviceInfo", State::Ready)?;
        let result = self.call_no_input(self.symbols.device_info);
        self.finish_read(result, DataShape::Device)
    }

    pub fn storages(&mut self) -> Result<Value> {
        self.require("FetchStorages", State::Ready)?;
        let result = self.call_no_input(self.symbols.storages);
        self.finish_read(result, DataShape::List)
    }

    pub fn walk(&mut self, request: &WalkRequest) -> Result<Value> {
        self.require("Walk", State::Ready)?;
        validate_remote_path(&request.full_path)?;
        let input = input_json(request)?;
        let result = self.call_with_input(self.symbols.walk, &input);
        self.finish_read(result, DataShape::List)
    }

    pub fn exists(&mut self, request: &ExistsRequest) -> Result<Value> {
        self.require("FileExists", State::Ready)?;
        if request.files.is_empty() {
            return Err(KalamError::Input("files must not be empty".into()));
        }
        for path in &request.files {
            validate_remote_path(path)?;
        }
        let input = input_json(request)?;
        let result = self.call_with_input(self.symbols.exists, &input);
        self.finish_read(result, DataShape::List)
    }

    /// Explicit cleanup. Native errors are returned, never inferred from names.
    /// Terminal even on failure: do not retry uncertain native cleanup.
    pub fn dispose(&mut self) -> Result<Value> {
        if self.state == State::Closed {
            return Err(KalamError::State {
                operation: "Dispose",
                state: self.state.name(),
            });
        }
        self.state = State::Closed;
        if !self.initialize_attempted {
            // No native initialization was attempted: no USB resources to close.
            return Ok(serde_json::json!({"errorType":"", "error":"", "data":true}));
        }
        let result = self.call_no_input(self.symbols.dispose);
        result.and_then(|value| validate_data(value, DataShape::True))
    }

    fn require(&self, operation: &'static str, state: State) -> Result<()> {
        if self.state != state {
            return Err(KalamError::State {
                operation,
                state: self.state.name(),
            });
        }
        Ok(())
    }

    fn finish_read(&mut self, result: Result<Value>, shape: DataShape) -> Result<Value> {
        // Any native failure can invalidate the old Go global device state.
        result
            .and_then(|value| validate_data(value, shape))
            .inspect_err(|_| self.state = State::Failed)
    }

    fn call_no_input(&mut self, function: NoInput) -> Result<Value> {
        begin_callback()?;
        // SAFETY: pinned exact symbol, synchronous callback contract, one owner.
        unsafe { function(callback_address()) };
        end_callback()
    }

    fn call_with_input(&mut self, function: WithInput, input: &CString) -> Result<Value> {
        begin_callback()?;
        // SAFETY: Go only reads the input during this synchronous call. CString
        // remains Rust-owned and valid through return; native must not free it.
        unsafe { function(input.as_ptr().cast_mut(), callback_address()) };
        end_callback()
    }
}

impl Drop for Kalam {
    fn drop(&mut self) {
        if self.initialize_attempted && self.state != State::Closed {
            // Last-resort cleanup only; worker should call explicit dispose to
            // observe errors. Rust panics never escape Drop; native faults can.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.dispose()));
        }
        // Intentionally do NOT drop _library or release the process owner.
    }
}

unsafe fn symbol<T: Copy>(library: &Library, bytes: &[u8], name: &'static str) -> Result<T> {
    // SAFETY: caller supplies a known ABI signature; Library remains pinned.
    unsafe { library.get::<T>(bytes) }
        .map(|value| *value)
        .map_err(|source| KalamError::MissingSymbol {
            symbol: name,
            source,
        })
}

fn callback_address() -> CallbackAddress {
    terminal_callback as *const () as *mut c_void
}

fn begin_callback() -> Result<()> {
    let mut slot = lock(&CALLBACK);
    if slot.unexpected || slot.active {
        return Err(KalamError::UnexpectedCallback);
    }
    slot.active = true;
    slot.count = 0;
    slot.result = None;
    Ok(())
}

fn end_callback() -> Result<Value> {
    let mut slot = lock(&CALLBACK);
    slot.active = false;
    if slot.unexpected {
        return Err(KalamError::UnexpectedCallback);
    }
    if slot.count > 1 {
        return Err(KalamError::DuplicateCallback);
    }
    slot.result
        .take()
        .unwrap_or(Err(KalamError::MissingCallback))
        .and_then(validate_envelope)
}

fn begin_transfer_callback() -> Result<()> {
    begin_callback()?;
    let mut events = lock(&EVENT_CALLBACK);
    events.active = true;
    events.error = None;
    Ok(())
}

fn end_transfer_callback() -> Result<Value> {
    let event_error = {
        let mut events = lock(&EVENT_CALLBACK);
        events.active = false;
        events.error.take()
    };
    // A terminal native error (especially ErrorNeedsReview) retains its typed
    // uncertainty metadata even if an event also failed. Events cannot succeed
    // a transfer or replace/count as the unique terminal result.
    end_callback().and_then(|value| match event_error {
        Some(error) => Err(error),
        None => Ok(value),
    })
}

extern "C" fn preprocess_callback(payload: *mut c_char) {
    event_callback(payload);
}
extern "C" fn progress_callback(payload: *mut c_char) {
    event_callback(payload);
}

fn event_callback(payload: *mut c_char) {
    let _owned = OwnedPayload(payload);
    let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let result = if payload.is_null() {
            Err(KalamError::NullCallback)
        } else {
            // SAFETY: same trusted C.CString transfer contract and cap as
            // terminal payloads; result is Rust-owned, then discarded.
            unsafe { copy_json(payload) }.and_then(validate_envelope)
        };
        let terminal = lock(&CALLBACK);
        let mut events = lock(&EVENT_CALLBACK);
        if !terminal.active || !events.active {
            drop(events);
            drop(terminal);
            lock(&CALLBACK).unexpected = true;
            return;
        }
        if terminal.count != 0 && events.error.is_none() {
            events.error = Some(KalamError::Envelope(
                "event callback arrived after terminal result".into(),
            ));
        }
        if let Err(error) = result
            && events.error.is_none()
        {
            events.error = Some(error);
        }
    }));
    if captured.is_err() {
        let terminal = lock(&CALLBACK);
        let mut events = lock(&EVENT_CALLBACK);
        if terminal.active && events.active {
            events.error = Some(KalamError::CallbackPanic);
        } else {
            drop(events);
            drop(terminal);
            lock(&CALLBACK).unexpected = true;
        }
    }
}

struct OwnedPayload(*mut c_char);
impl Drop for OwnedPayload {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Kalam transfers each distinct C.CString (malloc-family)
            // allocation to the callback, including error/duplicate payloads.
            unsafe { free(self.0.cast()) };
            #[cfg(test)]
            FREED_PAYLOADS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

extern "C" fn terminal_callback(payload: *mut c_char) {
    // Keep ownership outside catch_unwind: panic paths still release the string.
    let _owned = OwnedPayload(payload);
    let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let result = if payload.is_null() {
            Err(KalamError::NullCallback)
        } else {
            // SAFETY: trusted C.CString is NUL terminated and readable until NUL.
            // We inspect no more than MAX+1 bytes, and copy at most MAX bytes.
            unsafe { copy_json(payload) }
        };
        let mut slot = lock(&CALLBACK);
        if !slot.active {
            slot.unexpected = true;
            return;
        }
        slot.count = slot.count.saturating_add(1);
        if slot.count == 1 {
            slot.result = Some(result);
        }
    }));
    if captured.is_err() {
        let mut slot = lock(&CALLBACK);
        if slot.active {
            slot.count = slot.count.saturating_add(1);
            slot.result = Some(Err(KalamError::CallbackPanic));
        } else {
            slot.unexpected = true;
        }
    }
}

unsafe fn copy_json(payload: *const c_char) -> Result<Value> {
    let mut length = 0;
    while length <= MAX_CALLBACK_BYTES {
        // SAFETY: caller guarantees a valid NUL-terminated allocation.
        if unsafe { *payload.add(length) } == 0 {
            // SAFETY: the prefix has been read, is allocated, and has no NUL.
            let bytes =
                unsafe { std::slice::from_raw_parts(payload.cast::<u8>(), length) }.to_vec();
            return serde_json::from_slice(&bytes).map_err(KalamError::Json);
        }
        length += 1;
    }
    Err(KalamError::CallbackTooLarge)
}

fn validate_envelope(value: Value) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| KalamError::Envelope("expected an object".into()))?;
    let message = object
        .get("error")
        .and_then(Value::as_str)
        .ok_or_else(|| KalamError::Envelope("error must be a string".into()))?;
    let error_type = object
        .get("errorType")
        .and_then(Value::as_str)
        .ok_or_else(|| KalamError::Envelope("errorType must be a string".into()))?;
    if !message.is_empty() || !error_type.is_empty() {
        return Err(KalamError::Native {
            error_type: error_type.to_owned(),
            message: message.to_owned(),
            data: object.get("data").filter(|data| !data.is_null()).cloned(),
        });
    }
    if !object.contains_key("data") {
        return Err(KalamError::Envelope("data is missing".into()));
    }
    Ok(value)
}

enum DataShape {
    Device,
    List,
    True,
}
fn validate_data(value: Value, shape: DataShape) -> Result<Value> {
    let data = &value["data"];
    let valid = match shape {
        DataShape::Device => {
            data.is_object()
                && data.get("mtpDeviceInfo").is_some_and(Value::is_object)
                && data.get("usbDeviceInfo").is_some_and(Value::is_object)
        }
        // The Go shim serializes a nil slice as null, meaning no entries.
        DataShape::List => data.is_array() || data.is_null(),
        DataShape::True => data == &Value::Bool(true),
    };
    if !valid {
        return Err(KalamError::Envelope(
            "unexpected operation data shape".into(),
        ));
    }
    Ok(value)
}

fn input_json(request: &impl Serialize) -> Result<CString> {
    let bytes = serde_json::to_vec(request)?;
    if bytes.len() > MAX_CALLBACK_BYTES {
        return Err(KalamError::Input("request exceeds 1 MiB".into()));
    }
    CString::new(bytes).map_err(|_| KalamError::Input("request contains NUL".into()))
}

fn validate_remote_path(path: &str) -> Result<()> {
    if !path.starts_with('/')
        || path.contains(['\0', '\\'])
        || (path != "/"
            && path
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == ".."))
    {
        return Err(KalamError::Input(
            "remote path must be canonical absolute POSIX path without NUL, backslash or traversal"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
static FREED_PAYLOADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
mod inspect_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transfer_tests;
