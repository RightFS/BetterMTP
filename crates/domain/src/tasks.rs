use serde::Serialize;
use thiserror::Error;

use crate::{CopySnapshot, DirectoryRef, DomainError, EntryRef, checked_total_size};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TaskState {
    Queued,
    Running,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
    NeedsReview,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::NeedsReview
        )
    }
}

/// Report only what the backend can establish. Cancellation acknowledgement
/// means the operation has stopped; a cancellation request alone is not an outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ItemOutcome {
    Success,
    Failure { message: String, retryable: bool },
    Cancelled,
    UnknownOutcome { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskError {
    #[error("invalid {action} transition from {state:?}")]
    InvalidTransition {
        state: TaskState,
        action: &'static str,
    },
    #[error("job must contain at least one queued item")]
    EmptyJob,
    #[error("job construction requires fresh queued items")]
    ItemNotQueued,
    #[error("snapshot entry count differs from size metadata count")]
    SizeCountMismatch,
    #[error("item index {0} is outside this job")]
    InvalidItemIndex(usize),
    #[error("queue is stopped; currently running work is not automatically cancelled")]
    QueueStopped,
    #[error("job contains an unknown outcome; review is required before further work")]
    NeedsReview,
    #[error("another item is already active")]
    ItemAlreadyActive,
    #[error(transparent)]
    Domain(#[from] DomainError),
}

/// Terminal states cannot be rewritten. A retry must create a new item, retaining
/// this item's outcome as history. No public mutable fields or Deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    source: EntryRef,
    destination: DirectoryRef,
    size_bytes: u64,
    state: TaskState,
    outcome: Option<ItemOutcome>,
}

impl Item {
    pub fn new(source: EntryRef, destination: DirectoryRef, size_bytes: u64) -> Self {
        Self {
            source,
            destination,
            size_bytes,
            state: TaskState::Queued,
            outcome: None,
        }
    }
    pub fn source(&self) -> &EntryRef {
        &self.source
    }
    pub fn destination(&self) -> &DirectoryRef {
        &self.destination
    }
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
    pub fn state(&self) -> TaskState {
        self.state
    }
    pub fn outcome(&self) -> Option<&ItemOutcome> {
        self.outcome.as_ref()
    }

    pub fn start(&mut self) -> Result<(), TaskError> {
        if self.state != TaskState::Queued {
            return Err(self.invalid("start"));
        }
        self.state = TaskState::Running;
        Ok(())
    }

    pub fn request_cancel(&mut self) -> Result<(), TaskError> {
        match self.state {
            TaskState::Queued => {
                self.state = TaskState::Cancelled;
                self.outcome = Some(ItemOutcome::Cancelled);
            }
            TaskState::Running => self.state = TaskState::CancelRequested,
            TaskState::CancelRequested => {}
            _ => return Err(self.invalid("request_cancel")),
        }
        Ok(())
    }

    pub fn finish(&mut self, outcome: ItemOutcome) -> Result<(), TaskError> {
        if !matches!(self.state, TaskState::Running | TaskState::CancelRequested) {
            return Err(self.invalid("finish"));
        }
        // A success after cancellation is valid only if the backend confirmed
        // completion. Never infer success from stop, disconnect, or timeout.
        self.state = match &outcome {
            ItemOutcome::Success => TaskState::Completed,
            ItemOutcome::Failure { .. } => TaskState::Failed,
            ItemOutcome::Cancelled => TaskState::Cancelled,
            ItemOutcome::UnknownOutcome { .. } => TaskState::NeedsReview,
        };
        self.outcome = Some(outcome);
        Ok(())
    }

    pub fn can_auto_retry(&self) -> bool {
        self.state == TaskState::Failed
            && matches!(
                &self.outcome,
                Some(ItemOutcome::Failure {
                    retryable: true,
                    ..
                })
            )
    }

    fn invalid(&self, action: &'static str) -> TaskError {
        TaskError::InvalidTransition {
            state: self.state,
            action,
        }
    }
}

/// Sequential queue policy is separate from cancellation of the active item.
/// Aggregate state is derived, so it cannot falsely claim completion while any
/// child remains active or queued. NeedsReview dominates blocked queued work
/// and other terminal results; it prohibits any further dispatch. Pending work
/// can still be explicitly cancelled with stop_queue after an unknown outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Job {
    items: Vec<Item>,
    queue_stopped: bool,
    total_size_bytes: u64,
}

impl Job {
    pub fn new(items: Vec<Item>) -> Result<Self, TaskError> {
        if items.is_empty() {
            return Err(TaskError::EmptyJob);
        }
        if items.iter().any(|item| item.state != TaskState::Queued) {
            return Err(TaskError::ItemNotQueued);
        }
        let total_size_bytes = checked_total_size(items.iter().map(Item::size_bytes))?;
        Ok(Self {
            items,
            queue_stopped: false,
            total_size_bytes,
        })
    }

    /// Caller supplies sizes from captured metadata. Mismatched cardinality is
    /// an error, not a silently truncated zip and thus not a partial fake job.
    pub fn from_copy_snapshot(snapshot: &CopySnapshot, sizes: &[u64]) -> Result<Self, TaskError> {
        if snapshot.selection().len() != sizes.len() {
            return Err(TaskError::SizeCountMismatch);
        }
        Self::new(
            snapshot
                .selection()
                .iter()
                .zip(sizes)
                .map(|(source, size)| {
                    Item::new(source.clone(), snapshot.destination().clone(), *size)
                })
                .collect(),
        )
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub fn total_size_bytes(&self) -> u64 {
        self.total_size_bytes
    }
    pub fn queue_stopped(&self) -> bool {
        self.queue_stopped
    }

    pub fn state(&self) -> TaskState {
        if self
            .items
            .iter()
            .any(|item| item.state == TaskState::CancelRequested)
        {
            return TaskState::CancelRequested;
        }
        if self
            .items
            .iter()
            .any(|item| item.state == TaskState::Running)
        {
            return TaskState::Running;
        }
        if self
            .items
            .iter()
            .any(|item| item.state == TaskState::NeedsReview)
        {
            return TaskState::NeedsReview;
        }
        if self
            .items
            .iter()
            .any(|item| item.state == TaskState::Queued)
        {
            return TaskState::Queued;
        }
        for state in [TaskState::Failed, TaskState::Cancelled] {
            if self.items.iter().any(|item| item.state == state) {
                return state;
            }
        }
        TaskState::Completed
    }

    pub fn start_next(&mut self) -> Result<Option<usize>, TaskError> {
        if self
            .items
            .iter()
            .any(|item| item.state == TaskState::NeedsReview)
        {
            return Err(TaskError::NeedsReview);
        }
        if self.queue_stopped {
            return Err(TaskError::QueueStopped);
        }
        if self
            .items
            .iter()
            .any(|item| matches!(item.state, TaskState::Running | TaskState::CancelRequested))
        {
            return Err(TaskError::ItemAlreadyActive);
        }
        if let Some((index, item)) = self
            .items
            .iter_mut()
            .enumerate()
            .find(|(_, item)| item.state == TaskState::Queued)
        {
            item.start()?;
            return Ok(Some(index));
        }
        Ok(None)
    }

    pub fn request_item_cancel(&mut self, index: usize) -> Result<(), TaskError> {
        self.items
            .get_mut(index)
            .ok_or(TaskError::InvalidItemIndex(index))?
            .request_cancel()
    }

    pub fn finish_item(&mut self, index: usize, outcome: ItemOutcome) -> Result<(), TaskError> {
        self.items
            .get_mut(index)
            .ok_or(TaskError::InvalidItemIndex(index))?
            .finish(outcome)
    }

    /// Stops pending work only. Running work retains its current state until the
    /// caller explicitly requests cancellation and/or reports its actual outcome.
    pub fn stop_queue(&mut self) -> Result<(), TaskError> {
        if self.items.iter().all(|item| item.state.is_terminal()) {
            return Err(TaskError::InvalidTransition {
                state: self.state(),
                action: "stop_queue",
            });
        }
        for item in &mut self.items {
            if item.state == TaskState::Queued {
                item.request_cancel()?;
            }
        }
        self.queue_stopped = true;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn item(size: u64) -> Item {
        Item::new(
            EntryRef::Local("source".into()),
            DirectoryRef::Local("destination".into()),
            size,
        )
    }
    fn failure(retryable: bool) -> ItemOutcome {
        ItemOutcome::Failure {
            message: "failed".into(),
            retryable,
        }
    }
    fn unknown() -> ItemOutcome {
        ItemOutcome::UnknownOutcome {
            message: "connection lost during write".into(),
        }
    }

    #[test]
    fn item_starts_queued() {
        let i = item(1);
        assert_eq!(i.state(), TaskState::Queued);
        assert_eq!(i.outcome(), None);
    }
    #[test]
    fn queued_item_cannot_claim_completion() {
        let mut i = item(1);
        assert!(i.finish(ItemOutcome::Success).is_err());
        assert_eq!(i.state(), TaskState::Queued);
    }
    #[test]
    fn item_start_is_not_repeatable() {
        let mut i = item(1);
        i.start().unwrap();
        assert!(i.start().is_err());
        assert_eq!(i.state(), TaskState::Running);
    }
    #[test]
    fn running_cancel_is_request_not_acknowledgement() {
        let mut i = item(1);
        i.start().unwrap();
        i.request_cancel().unwrap();
        assert_eq!(i.state(), TaskState::CancelRequested);
        assert_eq!(i.outcome(), None);
    }
    #[test]
    fn cancellation_request_is_idempotent() {
        let mut i = item(1);
        i.start().unwrap();
        i.request_cancel().unwrap();
        assert!(i.request_cancel().is_ok());
    }
    #[test]
    fn queued_cancel_has_no_running_side_effects() {
        let mut i = item(1);
        i.request_cancel().unwrap();
        assert_eq!(i.state(), TaskState::Cancelled);
        assert!(i.start().is_err());
    }
    #[test]
    fn acknowledged_cancellation_is_terminal() {
        let mut i = item(1);
        i.start().unwrap();
        i.request_cancel().unwrap();
        i.finish(ItemOutcome::Cancelled).unwrap();
        assert_eq!(i.state(), TaskState::Cancelled);
    }
    #[test]
    fn verified_success_can_win_cancellation_race() {
        let mut i = item(1);
        i.start().unwrap();
        i.request_cancel().unwrap();
        i.finish(ItemOutcome::Success).unwrap();
        assert_eq!(i.state(), TaskState::Completed);
    }
    #[test]
    fn unknown_outcome_needs_review_and_never_auto_retries() {
        let mut i = item(1);
        i.start().unwrap();
        i.finish(unknown()).unwrap();
        assert_eq!(i.state(), TaskState::NeedsReview);
        assert!(!i.can_auto_retry());
        assert!(i.start().is_err());
    }
    #[test]
    fn disconnect_during_cancel_does_not_claim_cancelled() {
        let mut i = item(1);
        i.start().unwrap();
        i.request_cancel().unwrap();
        i.finish(unknown()).unwrap();
        assert_eq!(i.state(), TaskState::NeedsReview);
    }
    #[test]
    fn retry_requires_explicit_known_retryable_failure() {
        for retryable in [false, true] {
            let mut i = item(1);
            i.start().unwrap();
            i.finish(failure(retryable)).unwrap();
            assert_eq!(i.can_auto_retry(), retryable);
            assert!(i.start().is_err());
        }
    }
    #[test]
    fn every_terminal_outcome_is_immutable() {
        for outcome in [
            ItemOutcome::Success,
            failure(true),
            ItemOutcome::Cancelled,
            unknown(),
        ] {
            let mut i = item(1);
            i.start().unwrap();
            i.finish(outcome).unwrap();
            let before = i.clone();
            assert!(i.finish(ItemOutcome::Success).is_err());
            assert!(i.request_cancel().is_err());
            assert!(i.start().is_err());
            assert_eq!(i, before);
        }
    }
    #[test]
    fn job_rejects_empty() {
        assert_eq!(Job::new(vec![]), Err(TaskError::EmptyJob));
    }
    #[test]
    fn job_rejects_reused_item() {
        let mut i = item(1);
        i.start().unwrap();
        assert_eq!(Job::new(vec![i]), Err(TaskError::ItemNotQueued));
    }
    #[test]
    fn job_size_total_is_checked() {
        assert_eq!(
            Job::new(vec![item(u64::MAX), item(1)]),
            Err(TaskError::Domain(DomainError::SizeOverflow))
        );
    }
    #[test]
    fn job_size_total_preserves_u64() {
        assert_eq!(
            Job::new(vec![item(u64::MAX - 1), item(1)])
                .unwrap()
                .total_size_bytes(),
            u64::MAX
        );
    }
    #[test]
    fn job_starts_one_item_at_a_time() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        assert_eq!(j.start_next(), Ok(Some(0)));
        assert_eq!(j.start_next(), Err(TaskError::ItemAlreadyActive));
    }
    #[test]
    fn cancelling_item_still_blocks_queue() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        j.start_next().unwrap();
        j.request_item_cancel(0).unwrap();
        assert_eq!(j.start_next(), Err(TaskError::ItemAlreadyActive));
    }
    #[test]
    fn stop_queue_does_not_cancel_current() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        j.start_next().unwrap();
        j.stop_queue().unwrap();
        assert_eq!(j.items()[0].state(), TaskState::Running);
        assert_eq!(j.items()[1].state(), TaskState::Cancelled);
        assert_eq!(j.state(), TaskState::Running);
        assert_eq!(j.start_next(), Err(TaskError::QueueStopped));
        j.finish_item(0, ItemOutcome::Success).unwrap();
        assert_eq!(j.state(), TaskState::Cancelled);
    }
    #[test]
    fn stop_pending_queue_does_not_fake_success() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        j.stop_queue().unwrap();
        assert_eq!(j.state(), TaskState::Cancelled);
    }
    #[test]
    fn stopping_active_queue_is_idempotent() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        j.start_next().unwrap();
        j.stop_queue().unwrap();
        assert!(j.stop_queue().is_ok());
    }
    #[test]
    fn job_completion_requires_all_items() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        j.start_next().unwrap();
        j.finish_item(0, ItemOutcome::Success).unwrap();
        assert_eq!(j.state(), TaskState::Queued);
        assert_eq!(j.start_next(), Ok(Some(1)));
        assert_eq!(j.state(), TaskState::Running);
        j.finish_item(1, ItemOutcome::Success).unwrap();
        assert_eq!(j.state(), TaskState::Completed);
        assert_eq!(j.start_next(), Ok(None));
    }
    #[test]
    fn aggregate_unknown_dominates_failure_and_cancel() {
        let mut j = Job::new(vec![item(1), item(2), item(3)]).unwrap();
        j.start_next().unwrap();
        j.finish_item(0, failure(false)).unwrap();
        j.start_next().unwrap();
        j.finish_item(1, unknown()).unwrap();
        assert_eq!(j.state(), TaskState::NeedsReview);
        let before = j.clone();
        assert_eq!(j.start_next(), Err(TaskError::NeedsReview));
        assert_eq!(j, before);
        j.stop_queue().unwrap();
        assert_eq!(j.items()[2].state(), TaskState::Cancelled);
        assert_eq!(j.state(), TaskState::NeedsReview);
    }
    #[test]
    fn unknown_outcome_blocks_remaining_queue() {
        let mut j = Job::new(vec![item(1), item(2)]).unwrap();
        j.start_next().unwrap();
        j.finish_item(0, unknown()).unwrap();
        let before = j.clone();
        assert_eq!(j.start_next(), Err(TaskError::NeedsReview));
        assert_eq!(j, before);
        assert_eq!(j.items()[1].state(), TaskState::Queued);
        assert_eq!(j.state(), TaskState::NeedsReview);
        assert!(j.finish_item(1, ItemOutcome::Success).is_err());
    }
    #[test]
    fn aggregate_failure_is_not_success() {
        let mut j = Job::new(vec![item(1)]).unwrap();
        j.start_next().unwrap();
        j.finish_item(0, failure(false)).unwrap();
        assert_eq!(j.state(), TaskState::Failed);
    }
    #[test]
    fn terminal_job_rejects_changes() {
        let mut j = Job::new(vec![item(1)]).unwrap();
        j.stop_queue().unwrap();
        let before = j.clone();
        assert!(j.stop_queue().is_err());
        assert!(j.finish_item(0, ItemOutcome::Success).is_err());
        assert_eq!(j, before);
    }
    #[test]
    fn invalid_index_returns_error_without_mutation() {
        let mut j = Job::new(vec![item(1)]).unwrap();
        let before = j.clone();
        assert_eq!(
            j.request_item_cancel(99),
            Err(TaskError::InvalidItemIndex(99))
        );
        assert_eq!(
            j.finish_item(99, ItemOutcome::Success),
            Err(TaskError::InvalidItemIndex(99))
        );
        assert_eq!(j, before);
    }
    #[test]
    fn copy_job_uses_captured_destination() {
        let buffer =
            crate::CopyBuffer::capture(&[EntryRef::Local("a".into()), EntryRef::Local("b".into())])
                .unwrap();
        let snapshot = buffer.snapshot(&DirectoryRef::Local("captured".into()));
        let j = Job::from_copy_snapshot(&snapshot, &[1, 2]).unwrap();
        assert_eq!(j.total_size_bytes(), 3);
        assert!(
            j.items()
                .iter()
                .all(|i| i.destination() == snapshot.destination())
        );
    }
    #[test]
    fn snapshot_size_mismatch_is_not_silently_truncated() {
        let buffer = crate::CopyBuffer::capture(&[EntryRef::Local("a".into())]).unwrap();
        let snapshot = buffer.snapshot(&DirectoryRef::Local("dest".into()));
        assert_eq!(
            Job::from_copy_snapshot(&snapshot, &[]),
            Err(TaskError::SizeCountMismatch)
        );
        assert_eq!(
            Job::from_copy_snapshot(&snapshot, &[1, 2]),
            Err(TaskError::SizeCountMismatch)
        );
    }
}
