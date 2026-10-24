//! Single-flight registry for long-running local server file operations
//! (backup, verify, restore, delete).
//!
//! Only one operation may run at a time. Progress counters are updated by the
//! worker as it does real work, so the UI can only ever show measured progress.
//! Cancellation is serialized with the commit point through one mutex: a cancel
//! request either lands before the commit (and the worker cleans up) or is
//! rejected because the operation can no longer be cancelled safely.

use once_cell::sync::Lazy;
use serde::Serialize;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationKind {
    Backup,
    Verify,
    Restore,
    Delete,
    Download,
    Import,
    Export,
}

impl OperationKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Verify => "verification",
            Self::Restore => "restore",
            Self::Delete => "deletion",
            Self::Download => "server software download",
            Self::Import => "world import",
            Self::Export => "world export",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationState {
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// Why an operation stopped before completing.
#[derive(Debug, PartialEq, Eq)]
pub enum OpError {
    Cancelled,
    Failed(String),
}

impl From<String> for OpError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<OpError> for String {
    fn from(error: OpError) -> Self {
        match error {
            OpError::Cancelled => "The operation was cancelled.".into(),
            OpError::Failed(message) => message,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSnapshot {
    pub id: u64,
    pub kind: OperationKind,
    pub state: OperationState,
    pub phase: String,
    pub phase_index: u32,
    pub phase_count: u32,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_files: u64,
    pub total_files: u64,
    pub current: Option<String>,
    pub cancellable: bool,
    pub cancel_requested: bool,
    pub message: Option<String>,
    pub error: Option<String>,
}

struct Slot {
    next_id: u64,
    active: Option<Arc<Mutex<OperationSnapshot>>>,
    last: Option<OperationSnapshot>,
}

pub struct Registry {
    slot: Mutex<Slot>,
}

static GLOBAL: Lazy<Arc<Registry>> = Lazy::new(Registry::new);

pub fn global() -> Arc<Registry> {
    GLOBAL.clone()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Registry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(Slot {
                next_id: 1,
                active: None,
                last: None,
            }),
        })
    }

    /// Claims the single operation slot or explains what is already running.
    pub fn begin(
        self: &Arc<Self>,
        kind: OperationKind,
    ) -> Result<Tracker, String> {
        let mut slot = lock(&self.slot);
        if let Some(active) = &slot.active {
            let running = lock(active);
            return Err(format!(
                "A {} is already in progress. Wait for it to finish or cancel it first.",
                running.kind.label()
            ));
        }
        let id = slot.next_id;
        slot.next_id += 1;
        let shared = Arc::new(Mutex::new(OperationSnapshot {
            id,
            kind,
            state: OperationState::Running,
            phase: "Starting".into(),
            phase_index: 1,
            phase_count: 1,
            done_bytes: 0,
            total_bytes: 0,
            done_files: 0,
            total_files: 0,
            current: None,
            cancellable: false,
            cancel_requested: false,
            message: None,
            error: None,
        }));
        slot.active = Some(shared.clone());
        Ok(Tracker {
            registry: self.clone(),
            shared,
            finished: false,
        })
    }

    /// The kind of the operation currently holding the slot, if any.
    pub fn busy(&self) -> Option<OperationKind> {
        let slot = lock(&self.slot);
        slot.active.as_ref().map(|active| lock(active).kind)
    }

    /// The running operation, or the most recently finished one.
    pub fn snapshot(&self) -> Option<OperationSnapshot> {
        let slot = lock(&self.slot);
        match &slot.active {
            Some(active) => Some(lock(active).clone()),
            None => slot.last.clone(),
        }
    }

    /// Requests cancellation. Fails when nothing is running or the operation is
    /// past the point where stopping it would be unsafe.
    pub fn request_cancel(&self) -> Result<OperationSnapshot, String> {
        let slot = lock(&self.slot);
        let Some(active) = &slot.active else {
            return Err("There is no running operation to cancel.".into());
        };
        let mut snapshot = lock(active);
        if !snapshot.cancellable {
            return Err(format!(
                "The {} cannot be cancelled during '{}' because stopping now could leave data in an unsafe state.",
                snapshot.kind.label(),
                snapshot.phase
            ));
        }
        snapshot.cancel_requested = true;
        Ok(snapshot.clone())
    }
}

/// Held by the worker for the lifetime of one operation. Dropping it without
/// calling `finish` records a failure and always releases the slot.
pub struct Tracker {
    registry: Arc<Registry>,
    shared: Arc<Mutex<OperationSnapshot>>,
    finished: bool,
}

impl Tracker {
    /// Starts a new measurable phase. Counters restart from zero.
    pub fn phase(
        &self,
        name: &str,
        index: u32,
        count: u32,
        total_bytes: u64,
        total_files: u64,
        cancellable: bool,
    ) {
        let mut snapshot = lock(&self.shared);
        snapshot.phase = name.into();
        snapshot.phase_index = index;
        snapshot.phase_count = count;
        snapshot.total_bytes = total_bytes;
        snapshot.total_files = total_files;
        snapshot.done_bytes = 0;
        snapshot.done_files = 0;
        snapshot.current = None;
        snapshot.cancellable = cancellable;
    }

    pub fn add_bytes(&self, bytes: u64) {
        lock(&self.shared).done_bytes += bytes;
    }

    pub fn file_started(&self, relative: &str) {
        lock(&self.shared).current = Some(relative.into());
    }

    pub fn file_done(&self) {
        lock(&self.shared).done_files += 1;
    }

    /// Returns `Cancelled` once a cancel request was accepted.
    pub fn check_cancel(&self) -> Result<(), OpError> {
        if lock(&self.shared).cancel_requested {
            Err(OpError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// The commit point. Atomically either honors a pending cancel request or
    /// makes the operation uncancellable, so cancel can never race the commit.
    pub fn begin_commit(&self, phase: &str) -> Result<(), OpError> {
        let mut snapshot = lock(&self.shared);
        if snapshot.cancel_requested {
            return Err(OpError::Cancelled);
        }
        snapshot.cancellable = false;
        snapshot.phase = phase.into();
        snapshot.current = None;
        Ok(())
    }

    /// Records the outcome and releases the slot. Call only after any cleanup
    /// has finished so a new operation cannot start mid-cleanup.
    pub fn finish<T>(
        mut self,
        result: &Result<T, OpError>,
        success_message: &str,
    ) {
        self.record(match result {
            Ok(_) => (OperationState::Completed, Some(success_message), None),
            Err(OpError::Cancelled) => (
                OperationState::Cancelled,
                Some("Cancelled. Incomplete data was removed."),
                None,
            ),
            Err(OpError::Failed(message)) => {
                (OperationState::Failed, None, Some(message.as_str()))
            }
        });
    }

    fn record(
        &mut self,
        outcome: (OperationState, Option<&str>, Option<&str>),
    ) {
        let (state, message, error) = outcome;
        let finished = {
            let mut snapshot = lock(&self.shared);
            snapshot.state = state;
            snapshot.cancellable = false;
            snapshot.current = None;
            snapshot.message = message.map(str::to_string);
            snapshot.error = error.map(str::to_string);
            snapshot.clone()
        };
        let mut slot = lock(&self.registry.slot);
        slot.last = Some(finished);
        slot.active = None;
        self.finished = true;
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        if !self.finished {
            self.record((
                OperationState::Failed,
                None,
                Some("The operation ended unexpectedly."),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_operation_runs_at_a_time() {
        let registry = Registry::new();
        let first = registry.begin(OperationKind::Backup).unwrap();
        let error = registry.begin(OperationKind::Delete).err().unwrap();
        assert!(error.contains("backup is already in progress"));
        assert_eq!(registry.busy(), Some(OperationKind::Backup));
        first.finish(&Ok::<(), OpError>(()), "done");
        assert_eq!(registry.busy(), None);
        assert!(registry.begin(OperationKind::Delete).is_ok());
    }

    #[test]
    fn dropping_a_tracker_releases_the_slot_as_failed() {
        let registry = Registry::new();
        drop(registry.begin(OperationKind::Restore).unwrap());
        assert_eq!(registry.busy(), None);
        let last = registry.snapshot().unwrap();
        assert_eq!(last.state, OperationState::Failed);
    }

    #[test]
    fn cancel_is_only_accepted_while_cancellable() {
        let registry = Registry::new();
        assert!(registry.request_cancel().is_err());
        let tracker = registry.begin(OperationKind::Backup).unwrap();
        assert!(registry.request_cancel().is_err());
        tracker.phase("Copying", 1, 2, 10, 1, true);
        assert!(registry.request_cancel().unwrap().cancel_requested);
        assert_eq!(tracker.check_cancel(), Err(OpError::Cancelled));
    }

    #[test]
    fn commit_point_honors_a_prior_cancel_and_blocks_later_ones() {
        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Restore).unwrap();
        tracker.phase("Copying", 1, 2, 10, 1, true);
        registry.request_cancel().unwrap();
        assert_eq!(tracker.begin_commit("Finalizing"), Err(OpError::Cancelled));

        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Restore).unwrap();
        tracker.phase("Copying", 1, 2, 10, 1, true);
        tracker.begin_commit("Finalizing").unwrap();
        let error = registry.request_cancel().unwrap_err();
        assert!(error.contains("Finalizing"));
        assert!(!tracker.check_cancel().is_err());
    }

    #[test]
    fn progress_counters_reflect_recorded_work_only() {
        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Backup).unwrap();
        tracker.phase("Copying", 1, 1, 100, 4, true);
        tracker.file_started("region/r.0.0.mca");
        tracker.add_bytes(40);
        tracker.file_done();
        let snapshot = registry.snapshot().unwrap();
        assert_eq!(
            (
                snapshot.done_bytes,
                snapshot.total_bytes,
                snapshot.done_files
            ),
            (40, 100, 1)
        );
        assert_eq!(snapshot.current.as_deref(), Some("region/r.0.0.mca"));
        tracker.phase("Verifying", 2, 2, 50, 4, true);
        let snapshot = registry.snapshot().unwrap();
        assert_eq!((snapshot.done_bytes, snapshot.total_bytes), (0, 50));
    }

    #[test]
    fn finish_records_cancelled_and_failed_outcomes() {
        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Verify).unwrap();
        tracker.finish(&Err::<(), _>(OpError::Cancelled), "x");
        assert_eq!(
            registry.snapshot().unwrap().state,
            OperationState::Cancelled
        );
        let tracker = registry.begin(OperationKind::Verify).unwrap();
        tracker.finish(&Err::<(), _>(OpError::Failed("boom".into())), "x");
        let last = registry.snapshot().unwrap();
        assert_eq!(last.state, OperationState::Failed);
        assert_eq!(last.error.as_deref(), Some("boom"));
    }
}
