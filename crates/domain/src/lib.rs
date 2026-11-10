//! Pure domain types for the native MTP core. No transport, filesystem I/O or UI.
//!
//! A remote reference is meaningful only in its device session. Call
//! `validate_session` at the execution boundary, not only when selecting entries.
//! Task completion must report a verified outcome; a lost connection is an
//! `UnknownOutcome`, never success and never an automatic retry.

mod references;
mod selection;
mod tasks;

pub use references::*;
pub use selection::*;
pub use tasks::*;
