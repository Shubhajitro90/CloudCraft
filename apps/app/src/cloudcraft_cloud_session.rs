//! Provider-independent lifecycle manager for a remote cloud gaming session.
//!
//! This module owns the session state machine, duplicate-start prevention,
//! start cancellation, idempotent stop, timeouts, crash recovery, and
//! credential-free metadata persistence. Providers do the remote work: the
//! desktop app uses the self-hosted Sunshine/Moonlight provider
//! (`cloudcraft_cloud_sunshine`), and [`UnconfiguredProvider`] remains as the
//! inert provider for tests and builds without a host.
//!
//! Provider contract: `start` must be cancel-safe (dropping its future must not
//! leak resources it has not yet reported), and when it fails after resources
//! may exist it must return their identifier in [`ProviderError::remote_id`] so
//! the manager can retain it and require an explicit, successful `stop`.

use crate::cloudcraft_cloud_quota::{
    EndReason, QuotaLedger, QuotaPolicy, QuotaSnapshot, QuotaWarning,
};
use crate::cloudcraft_cloud_sunshine::{
    probe_certificate, ProviderView, SunshineConfig, SunshineProvider,
};
use chrono::Utc;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Seconds since the Unix epoch. Injected so quota limits are testable.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| Utc::now().timestamp())
}

/// How often the background task checks session health, records activity,
/// announces quota warnings, and enforces the session deadline.
const QUOTA_TICK: Duration = Duration::from_secs(15);
pub const QUOTA_WARNING_EVENT: &str = "cloudcraft-cloud-quota-warning";
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;

const METADATA_VERSION: u32 = 1;
const MAX_GAME_VERSION_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Unavailable,
    Idle,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Error,
}

impl SessionState {
    /// The single source of truth for legal transitions.
    pub fn can_transition_to(self, next: SessionState) -> bool {
        use SessionState::*;
        matches!(
            (self, next),
            (Unavailable, Idle)
                | (Idle, Starting)
                | (Idle, Unavailable)
                | (Stopped, Starting)
                | (Stopped, Idle)
                | (Stopped, Unavailable)
                | (Error, Starting)
                | (Error, Stopping)
                | (Error, Stopped)
                // Only through `reconnect`, which requires a recorded remote session.
                | (Error, Ready)
                | (Starting, Ready)
                | (Starting, Error)
                | (Starting, Stopped)
                | (Starting, Stopping)
                | (Ready, Stopping)
                | (Ready, Error)
                | (Stopping, Stopped)
                | (Stopping, Error)
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct StartRequest {
    pub game_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteSession {
    pub remote_id: String,
}

#[derive(Debug, Clone)]
pub struct ProviderError {
    pub message: String,
    /// Set when remote resources may exist and must be released.
    pub remote_id: Option<String>,
}

pub trait CloudSessionProvider: Send + Sync {
    fn id(&self) -> &'static str;
    /// `Some(reason)` while the provider cannot be used (not configured, no authorization).
    fn unavailable_reason(&self) -> Option<String>;
    fn start<'a>(
        &'a self,
        request: &'a StartRequest,
    ) -> BoxFuture<'a, Result<RemoteSession, ProviderError>>;
    fn stop<'a>(
        &'a self,
        remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
    /// `Some(problem)` when a ready session lost its local stream or remote side.
    fn health(&self, _remote_id: &str) -> Option<String> {
        None
    }
    fn supports_reconnect(&self) -> bool {
        false
    }
    /// Authoritative compute seconds used in the current quota week, when the
    /// provider exposes them. `None` keeps usage labelled as a local estimate.
    fn reported_weekly_usage_secs(&self) -> Option<u64> {
        None
    }
    /// Re-attaches the local stream to a remote session that is still allocated.
    fn reconnect<'a>(
        &'a self,
        _remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async {
            Err("This provider cannot reconnect to a session.".to_string())
        })
    }
}

/// Inert provider that never creates remote resources.
#[cfg_attr(not(test), allow(dead_code))]
pub struct UnconfiguredProvider;

impl CloudSessionProvider for UnconfiguredProvider {
    fn id(&self) -> &'static str {
        "none"
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some(
            "No cloud gaming provider is configured. Minecraft runs locally through the launcher; no remote resources can be created."
                .into(),
        )
    }

    fn start<'a>(
        &'a self,
        _request: &'a StartRequest,
    ) -> BoxFuture<'a, Result<RemoteSession, ProviderError>> {
        Box::pin(async {
            Err(ProviderError {
                message: "No cloud gaming provider is configured. No remote resources were created."
                    .into(),
                remote_id: None,
            })
        })
    }

    fn stop<'a>(
        &'a self,
        _remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async {
            Err("No cloud gaming provider is configured.".into())
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub start: Duration,
    pub stop: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            start: Duration::from_secs(600),
            stop: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub state: SessionState,
    pub message: String,
    pub session_id: Option<String>,
    pub provider_id: String,
    pub updated_at: Option<String>,
    pub can_start: bool,
    pub can_stop: bool,
    pub can_cancel: bool,
    pub can_reconnect: bool,
    /// True when remote resources may still exist and need a successful stop.
    pub cleanup_required: bool,
    /// Continuous-session and weekly usage. `can_start` already includes `quota.can_launch`.
    pub quota: QuotaSnapshot,
}

/// Persisted to disk. Contains no credentials, tokens, or account data.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedSession {
    version: u32,
    state: SessionState,
    message: String,
    session_id: Option<String>,
    remote_id: Option<String>,
    provider_id: String,
    game_version: Option<String>,
    updated_at: String,
}

#[derive(Default)]
struct StartControl {
    cancelled: AtomicBool,
    notify: Notify,
}

impl StartControl {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.notify.notified().await;
        }
    }
}

struct Inner {
    state: SessionState,
    message: String,
    session_id: Option<String>,
    remote_id: Option<String>,
    game_version: Option<String>,
    updated_at: Option<String>,
    control: Option<Arc<StartControl>>,
}

impl Inner {
    fn set_state(
        &mut self,
        next: SessionState,
        message: impl Into<String>,
    ) -> Result<(), String> {
        if !self.state.can_transition_to(next) {
            return Err(format!(
                "Invalid cloud session transition from {:?} to {:?}.",
                self.state, next
            ));
        }
        self.state = next;
        self.message = redact_secrets(&message.into());
        self.updated_at = Some(Utc::now().to_rfc3339());
        Ok(())
    }

    fn force_state(&mut self, next: SessionState, message: impl Into<String>) {
        if let Err(error) = self.set_state(next, message.into()) {
            tracing::error!("{error}");
        }
    }
}

enum StartOutcome {
    Started(RemoteSession),
    Failed(ProviderError),
    TimedOut,
    Cancelled,
}

/// Lock order: `inner` before `quota`, never the reverse.
pub struct SessionManager {
    inner: Mutex<Inner>,
    quota: Mutex<QuotaLedger>,
    provider: Arc<dyn CloudSessionProvider>,
    store_path: PathBuf,
    timeouts: Timeouts,
    clock: Clock,
}

impl SessionManager {
    /// Creates the manager and recovers any session left behind by a previous run.
    pub fn new(
        store_path: PathBuf,
        provider: Arc<dyn CloudSessionProvider>,
        timeouts: Timeouts,
    ) -> Self {
        Self::with_clock(store_path, provider, timeouts, system_clock())
    }

    pub fn with_clock(
        store_path: PathBuf,
        provider: Arc<dyn CloudSessionProvider>,
        timeouts: Timeouts,
        clock: Clock,
    ) -> Self {
        let mut inner = Inner {
            state: SessionState::Idle,
            message: "No cloud session.".into(),
            session_id: None,
            remote_id: None,
            game_version: None,
            updated_at: None,
            control: None,
        };
        Self::recover(&store_path, &mut inner);
        let now = clock();
        let mut quota = QuotaLedger::load(
            store_path.with_file_name("cloudcraft-cloud-quota.json"),
            now,
        );
        // A recorded remote id means the remote side may still be running, so it keeps counting.
        quota.recover(inner.remote_id.is_some(), now);
        let manager = Self {
            inner: Mutex::new(inner),
            quota: Mutex::new(quota),
            provider,
            store_path,
            timeouts,
            clock,
        };
        {
            let inner = manager.lock();
            manager.persist(&inner);
        }
        manager
    }

    fn recover(store_path: &PathBuf, inner: &mut Inner) {
        let Ok(contents) = fs::read_to_string(store_path) else {
            return;
        };
        let Ok(saved) = serde_json::from_str::<PersistedSession>(&contents)
        else {
            tracing::warn!("Ignoring unreadable cloud session metadata.");
            return;
        };
        if saved.version != METADATA_VERSION {
            return;
        }
        inner.session_id = saved.session_id;
        inner.game_version = saved.game_version;
        inner.updated_at = Some(saved.updated_at);
        match (saved.state, saved.remote_id) {
            // The previous run ended with a live or half-finished session: status is unknown.
            (
                SessionState::Starting
                | SessionState::Ready
                | SessionState::Stopping
                | SessionState::Error,
                Some(remote_id),
            ) => {
                inner.remote_id = Some(remote_id);
                inner.state = SessionState::Error;
                inner.message = "CloudCraft was closed while a remote session may still have been active. Its status is unknown; stop it to release remote resources.".into();
            }
            (SessionState::Starting | SessionState::Stopping, None) => {
                inner.state = SessionState::Stopped;
                inner.message = "The previous session start was interrupted before any remote resource was reported.".into();
            }
            (state, _) => {
                inner.state = match state {
                    SessionState::Unavailable | SessionState::Idle => {
                        SessionState::Idle
                    }
                    SessionState::Ready => SessionState::Stopped,
                    other => other,
                };
                inner.message = saved.message;
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_quota(&self) -> MutexGuard<'_, QuotaLedger> {
        self.quota
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn end_usage(&self, inner: &Inner, reason: EndReason) {
        if let Some(session_id) = inner.session_id.as_deref() {
            self.lock_quota().end(session_id, (self.clock)(), reason);
        }
    }

    /// Health check plus activity heartbeat. Only a healthy or starting
    /// session counts as confirmed activity for crash recovery.
    fn observe(&self, inner: &mut Inner) {
        self.refresh_availability(inner);
        if inner.state == SessionState::Ready {
            let problem = inner
                .remote_id
                .as_deref()
                .and_then(|id| self.provider.health(id));
            if let Some(problem) = problem {
                inner.force_state(SessionState::Error, problem);
                self.persist(inner);
            }
        }
        if matches!(inner.state, SessionState::Ready | SessionState::Starting) {
            self.lock_quota().heartbeat((self.clock)());
        }
    }

    pub fn quota_status(&self) -> QuotaSnapshot {
        self.lock_quota().snapshot((self.clock)())
    }

    pub fn set_quota_policy(
        &self,
        policy: QuotaPolicy,
    ) -> Result<QuotaSnapshot, String> {
        let now = (self.clock)();
        let mut quota = self.lock_quota();
        quota.set_policy(policy, now)?;
        Ok(quota.snapshot(now))
    }

    /// Periodic work: health, heartbeat, deadline enforcement. Returns warnings
    /// that have not been announced before (each is announced once).
    pub async fn tick(&self) -> Vec<QuotaWarning> {
        {
            let mut inner = self.lock();
            self.observe(&mut inner);
        }
        let reported = self.provider.reported_weekly_usage_secs();
        let warnings = {
            let now = (self.clock)();
            let mut quota = self.lock_quota();
            if let Some(used) = reported {
                quota.apply_provider_usage(used, now);
            }
            quota.take_new_warnings(now)
        };
        self.enforce().await;
        warnings
    }

    /// Stops the session once either limit is reached. CloudCraft cannot save
    /// a world inside the remote client; the warnings beforehand tell the
    /// player to save in-game, and the end record says progress may be lost.
    pub async fn enforce(&self) -> Option<SessionSnapshot> {
        let reason = self.lock_quota().enforcement((self.clock)())?;
        {
            let mut inner = self.lock();
            let message = match reason {
                EndReason::WeeklyAllowance => "The weekly cloud allowance ran out. Stopping the cloud session…",
                _ => "The continuous-session limit was reached. Stopping the cloud session…",
            };
            match inner.state {
                SessionState::Ready => {
                    inner.force_state(SessionState::Stopping, message)
                }
                SessionState::Error if inner.remote_id.is_some() => {
                    inner.force_state(SessionState::Stopping, message)
                }
                SessionState::Starting => {
                    if let Some(control) = inner.control.as_ref() {
                        control.cancel();
                    }
                    return Some(self.snapshot_of(&inner));
                }
                _ => {
                    // Nothing remote is held; close the counter so it cannot keep running.
                    let active = self
                        .lock_quota()
                        .active_session_id()
                        .map(str::to_string);
                    if let Some(id) = active {
                        self.lock_quota().end(&id, (self.clock)(), reason);
                    }
                    return Some(self.snapshot_of(&inner));
                }
            }
            self.persist(&inner);
        }
        Some(self.finish_stop(reason).await)
    }

    fn persist(&self, inner: &Inner) {
        let record = PersistedSession {
            version: METADATA_VERSION,
            state: inner.state,
            message: inner.message.clone(),
            session_id: inner.session_id.clone(),
            remote_id: inner.remote_id.clone(),
            provider_id: self.provider.id().to_string(),
            game_version: inner.game_version.clone(),
            updated_at: inner
                .updated_at
                .clone()
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
        };
        let write = || -> Result<(), String> {
            if let Some(parent) = self.store_path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let serialized = serde_json::to_string_pretty(&record)
                .map_err(|e| e.to_string())?;
            let temp = self.store_path.with_extension("json.tmp");
            fs::write(&temp, serialized).map_err(|e| e.to_string())?;
            fs::rename(&temp, &self.store_path).map_err(|e| e.to_string())
        };
        if let Err(error) = write() {
            tracing::warn!("Could not persist cloud session metadata: {error}");
        }
    }

    fn refresh_availability(&self, inner: &mut Inner) {
        let reason = self.provider.unavailable_reason();
        match (reason, inner.state) {
            (Some(reason), SessionState::Idle | SessionState::Stopped) => {
                inner.force_state(SessionState::Unavailable, reason);
            }
            (Some(reason), SessionState::Unavailable) => inner.message = reason,
            (None, SessionState::Unavailable) => {
                inner.force_state(
                    SessionState::Idle,
                    "Ready to start a cloud session.",
                );
            }
            _ => {}
        }
    }

    fn snapshot_of(&self, inner: &Inner) -> SessionSnapshot {
        let unavailable = self.provider.unavailable_reason().is_some();
        let cancel_requested =
            inner.control.as_ref().is_some_and(|c| c.is_cancelled());
        let quota = self.quota_status();
        let can_start = !unavailable
            && quota.can_launch
            && match inner.state {
                SessionState::Idle | SessionState::Stopped => true,
                SessionState::Error => inner.remote_id.is_none(),
                _ => false,
            };
        let can_stop = match inner.state {
            SessionState::Ready => true,
            SessionState::Error => inner.remote_id.is_some(),
            _ => false,
        };
        SessionSnapshot {
            state: inner.state,
            message: inner.message.clone(),
            session_id: inner.session_id.clone(),
            provider_id: self.provider.id().to_string(),
            updated_at: inner.updated_at.clone(),
            can_start,
            can_stop,
            can_cancel: inner.state == SessionState::Starting
                && inner.control.is_some()
                && !cancel_requested,
            can_reconnect: inner.state == SessionState::Error
                && inner.remote_id.is_some()
                && self.provider.supports_reconnect(),
            cleanup_required: inner.remote_id.is_some()
                && !matches!(
                    inner.state,
                    SessionState::Stopped | SessionState::Idle
                ),
            quota,
        }
    }

    pub fn status(&self) -> SessionSnapshot {
        let mut inner = self.lock();
        self.observe(&mut inner);
        self.snapshot_of(&inner)
    }

    /// Re-attaches to a remote session that is still allocated (Error with a remote id).
    pub async fn reconnect(&self) -> Result<SessionSnapshot, String> {
        let remote_id = {
            let inner = self.lock();
            match (&inner.state, &inner.remote_id) {
                (SessionState::Error, Some(id))
                    if self.provider.supports_reconnect() =>
                {
                    id.clone()
                }
                _ => {
                    return Err(
                        "There is no interrupted session to reconnect to."
                            .into(),
                    )
                }
            }
        };
        let result = tokio::time::timeout(
            self.timeouts.stop,
            self.provider.reconnect(&remote_id),
        )
        .await
        .unwrap_or_else(|_| Err("Reconnecting timed out.".into()));
        let mut inner = self.lock();
        if inner.state != SessionState::Error
            || inner.remote_id.as_deref() != Some(remote_id.as_str())
        {
            return Ok(self.snapshot_of(&inner));
        }
        match result {
            Ok(()) => inner.force_state(
                SessionState::Ready,
                "Reconnected to the cloud session.",
            ),
            Err(message) => {
                inner.message = redact_secrets(&format!(
                    "Reconnect failed: {message} Stop the session to release the remote computer."
                ));
            }
        }
        self.persist(&inner);
        Ok(self.snapshot_of(&inner))
    }

    pub async fn start(
        &self,
        request: StartRequest,
    ) -> Result<SessionSnapshot, String> {
        validate_request(&request)?;

        let control = {
            let mut inner = self.lock();
            self.refresh_availability(&mut inner);
            match inner.state {
                SessionState::Unavailable => return Err(inner.message.clone()),
                SessionState::Starting => {
                    return Err("A cloud session is already starting.".into())
                }
                SessionState::Ready => {
                    return Err("A cloud session is already running. Stop it first.".into())
                }
                SessionState::Stopping => {
                    return Err("The previous cloud session is still stopping.".into())
                }
                SessionState::Error if inner.remote_id.is_some() => {
                    return Err("The previous session may still hold remote resources. Stop it before starting a new one.".into())
                }
                _ => {}
            }
            // Eligibility is rechecked on every launch; this also starts both usage counters.
            let session_id = uuid::Uuid::new_v4().to_string();
            self.lock_quota().begin(&session_id, (self.clock)())?;
            if let Err(error) = inner
                .set_state(SessionState::Starting, "Starting cloud session…")
            {
                self.lock_quota().end(
                    &session_id,
                    (self.clock)(),
                    EndReason::StartFailed,
                );
                return Err(error);
            }
            inner.session_id = Some(session_id);
            inner.remote_id = None;
            inner.game_version = request.game_version.clone();
            let control = Arc::new(StartControl::default());
            inner.control = Some(control.clone());
            self.persist(&inner);
            control
        };

        let outcome = tokio::select! {
            biased;
            _ = control.cancelled() => StartOutcome::Cancelled,
            result = tokio::time::timeout(self.timeouts.start, self.provider.start(&request)) => {
                match result {
                    Ok(Ok(remote)) => StartOutcome::Started(remote),
                    Ok(Err(error)) => StartOutcome::Failed(error),
                    Err(_) => StartOutcome::TimedOut,
                }
            }
        };

        let release_after_cancel = {
            let mut inner = self.lock();
            inner.control = None;
            let release = match outcome {
                StartOutcome::Started(remote) => {
                    inner.remote_id = Some(remote.remote_id);
                    if control.is_cancelled() {
                        inner.force_state(
                            SessionState::Stopping,
                            "Start cancelled; releasing the remote session…",
                        );
                        true
                    } else {
                        inner.force_state(
                            SessionState::Ready,
                            "Cloud session is ready.",
                        );
                        false
                    }
                }
                StartOutcome::Failed(error) => {
                    inner.remote_id = error.remote_id;
                    inner.force_state(
                        SessionState::Error,
                        format!(
                            "Cloud session failed to start: {}",
                            error.message
                        ),
                    );
                    // Possibly-live remote resources keep counting until stopped.
                    if inner.remote_id.is_none() {
                        self.end_usage(&inner, EndReason::StartFailed);
                    }
                    false
                }
                StartOutcome::TimedOut => {
                    inner.force_state(
                        SessionState::Error,
                        "Cloud session start timed out. The provider may still be provisioning; check it for leftover resources.",
                    );
                    self.end_usage(&inner, EndReason::StartFailed);
                    false
                }
                StartOutcome::Cancelled => {
                    inner.force_state(
                        SessionState::Stopped,
                        "Start cancelled. The provider had not reported any remote resource.",
                    );
                    self.end_usage(&inner, EndReason::Cancelled);
                    false
                }
            };
            self.persist(&inner);
            release
        };

        if release_after_cancel {
            return Ok(self.finish_stop(EndReason::Cancelled).await);
        }
        Ok(self.status())
    }

    /// Requests cancellation of a start in progress. No-op in every other state.
    pub fn cancel_start(&self) -> SessionSnapshot {
        let inner = self.lock();
        if let (SessionState::Starting, Some(control)) =
            (inner.state, inner.control.as_ref())
        {
            control.cancel();
        }
        self.snapshot_of(&inner)
    }

    /// Idempotent: stopping an absent, stopped, or already-stopping session succeeds.
    pub async fn stop(&self) -> SessionSnapshot {
        {
            let mut inner = self.lock();
            match inner.state {
                SessionState::Unavailable
                | SessionState::Idle
                | SessionState::Stopped
                | SessionState::Stopping => return self.snapshot_of(&inner),
                SessionState::Starting => {
                    if let Some(control) = inner.control.as_ref() {
                        control.cancel();
                    }
                    return self.snapshot_of(&inner);
                }
                SessionState::Ready => {
                    inner.force_state(
                        SessionState::Stopping,
                        "Stopping cloud session…",
                    );
                }
                SessionState::Error => {
                    if inner.remote_id.is_some() {
                        inner.force_state(
                            SessionState::Stopping,
                            "Releasing remote resources…",
                        );
                    } else {
                        inner.force_state(
                            SessionState::Stopped,
                            "No remote resources were recorded for this session.",
                        );
                        self.end_usage(&inner, EndReason::Stopped);
                        self.persist(&inner);
                        return self.snapshot_of(&inner);
                    }
                }
            }
            self.persist(&inner);
        }
        self.finish_stop(EndReason::Stopped).await
    }

    /// Usage stops counting only once the provider confirms release; a failed
    /// stop keeps the counters running because the remote side may still be live.
    async fn finish_stop(&self, reason: EndReason) -> SessionSnapshot {
        let remote_id = self.lock().remote_id.clone();
        let result = match remote_id {
            None => Ok(()),
            Some(id) => match tokio::time::timeout(
                self.timeouts.stop,
                self.provider.stop(&id),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    Err("Releasing the remote session timed out.".to_string())
                }
            },
        };
        let mut inner = self.lock();
        match result {
            Ok(()) => {
                inner.remote_id = None;
                let message = match reason {
                    EndReason::SessionLimit => "Cloud session stopped: the continuous-session limit was reached. Progress not saved in Minecraft before the limit may be lost.",
                    EndReason::WeeklyAllowance => "Cloud session stopped: the weekly cloud allowance ran out. Progress not saved in Minecraft before the limit may be lost.",
                    _ => "Cloud session stopped.",
                };
                inner.force_state(SessionState::Stopped, message);
                self.end_usage(&inner, reason);
            }
            Err(message) => inner.force_state(
                SessionState::Error,
                format!("Could not release the remote session: {message} Retry stop to avoid leaked resources."),
            ),
        }
        self.persist(&inner);
        self.snapshot_of(&inner)
    }
}

fn validate_request(request: &StartRequest) -> Result<(), String> {
    if let Some(version) = &request.game_version {
        let valid = !version.is_empty()
            && version.len() <= MAX_GAME_VERSION_LEN
            && version.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')
            });
        if !valid {
            return Err(
                "The Minecraft version contains unsupported characters.".into(),
            );
        }
    }
    Ok(())
}

/// Removes bearer tokens and `token=`/`key=`/`secret=`/`password=` values from text.
pub fn redact_secrets(text: &str) -> String {
    let mut previous_was_bearer = false;
    text.split(' ')
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            let redacted = if previous_was_bearer {
                "[redacted]".to_string()
            } else if let Some((name, _)) = word.split_once('=') {
                let name_lower = name.to_ascii_lowercase();
                if ["token", "key", "secret", "password", "authorization"]
                    .iter()
                    .any(|sensitive| name_lower.contains(sensitive))
                {
                    format!("{name}=[redacted]")
                } else {
                    word.to_string()
                }
            } else {
                word.to_string()
            };
            previous_was_bearer = lower == "bearer";
            redacted
        })
        .collect::<Vec<_>>()
        .join(" ")
}

static MANAGER: once_cell::sync::OnceCell<SessionManager> =
    once_cell::sync::OnceCell::new();
static SUNSHINE: once_cell::sync::OnceCell<Arc<SunshineProvider>> =
    once_cell::sync::OnceCell::new();

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path().app_data_dir().map_err(|error| {
        format!("Could not locate the CloudCraft data directory: {error}")
    })
}

fn sunshine(
    app: &tauri::AppHandle,
) -> Result<&'static Arc<SunshineProvider>, String> {
    SUNSHINE.get_or_try_init(|| {
        Ok(Arc::new(SunshineProvider::load(
            data_dir(app)?.join("cloudcraft-cloud-host.json"),
        )))
    })
}

fn manager(app: &tauri::AppHandle) -> Result<&'static SessionManager, String> {
    let provider = sunshine(app)?.clone();
    MANAGER.get_or_try_init(|| {
        Ok(SessionManager::new(
            data_dir(app)?.join("cloudcraft-cloud-session.json"),
            provider,
            Timeouts::default(),
        ))
    })
}

/// Loads the provider at launcher start so a Moonlight process orphaned by a
/// previous crash is stopped immediately, and restores the session record.
pub fn init(app: &tauri::AppHandle) {
    if let Err(error) = manager(app) {
        tracing::warn!("CloudCraft cloud session init failed: {error}");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Emitter;
        loop {
            tokio::time::sleep(QUOTA_TICK).await;
            let Some(manager) = MANAGER.get() else {
                continue;
            };
            let warnings = manager.tick().await;
            if !warnings.is_empty() {
                if let Err(error) = app.emit(QUOTA_WARNING_EVENT, &warnings) {
                    tracing::warn!("Could not deliver quota warning: {error}");
                }
            }
        }
    });
}

/// Kills the local stream client on launcher exit.
pub fn shutdown() {
    if let Some(provider) = SUNSHINE.get() {
        provider.shutdown();
    }
}

fn session_is_active(app: &tauri::AppHandle) -> Result<bool, String> {
    let status = manager(app)?.status();
    Ok(status.cleanup_required
        || matches!(
            status.state,
            SessionState::Starting
                | SessionState::Ready
                | SessionState::Stopping
        ))
}

#[tauri::command]
pub fn cloudcraft_cloud_host_get(
    app: tauri::AppHandle,
) -> Result<ProviderView, String> {
    Ok(sunshine(&app)?.view())
}

#[tauri::command]
pub async fn cloudcraft_cloud_host_probe(
    host: String,
    api_port: u16,
) -> Result<String, String> {
    probe_certificate(&host, api_port).await
}

#[tauri::command]
pub fn cloudcraft_cloud_host_save(
    app: tauri::AppHandle,
    config: SunshineConfig,
) -> Result<ProviderView, String> {
    if session_is_active(&app)? {
        return Err(
            "Stop the cloud session before changing the remote host.".into()
        );
    }
    let provider = sunshine(&app)?;
    provider.save_config(config)?;
    Ok(provider.view())
}

#[tauri::command]
pub fn cloudcraft_cloud_host_remove(
    app: tauri::AppHandle,
) -> Result<ProviderView, String> {
    if session_is_active(&app)? {
        return Err(
            "Stop the cloud session before removing the remote host.".into()
        );
    }
    let provider = sunshine(&app)?;
    provider.clear_config()?;
    Ok(provider.view())
}

#[tauri::command]
pub async fn cloudcraft_cloud_host_unlock(
    app: tauri::AppHandle,
    password: String,
) -> Result<ProviderView, String> {
    let provider = sunshine(&app)?;
    provider.unlock(password).await?;
    Ok(provider.view())
}

#[tauri::command]
pub fn cloudcraft_cloud_host_lock(
    app: tauri::AppHandle,
) -> Result<ProviderView, String> {
    let provider = sunshine(&app)?;
    provider.lock_credentials();
    Ok(provider.view())
}

#[tauri::command]
pub async fn cloudcraft_cloud_host_ping(
    app: tauri::AppHandle,
) -> Result<u64, String> {
    sunshine(&app)?.ping().await
}

#[tauri::command]
pub fn cloudcraft_cloud_host_logs(
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    Ok(sunshine(&app)?.logs())
}

#[tauri::command]
pub async fn cloudcraft_cloud_session_reconnect(
    app: tauri::AppHandle,
) -> Result<SessionSnapshot, String> {
    manager(&app)?.reconnect().await
}

#[tauri::command]
pub fn cloudcraft_cloud_session_status(
    app: tauri::AppHandle,
) -> Result<SessionSnapshot, String> {
    Ok(manager(&app)?.status())
}

#[tauri::command]
pub async fn cloudcraft_cloud_session_start(
    app: tauri::AppHandle,
    game_version: Option<String>,
) -> Result<SessionSnapshot, String> {
    manager(&app)?.start(StartRequest { game_version }).await
}

#[tauri::command]
pub fn cloudcraft_cloud_session_cancel_start(
    app: tauri::AppHandle,
) -> Result<SessionSnapshot, String> {
    Ok(manager(&app)?.cancel_start())
}

#[tauri::command]
pub async fn cloudcraft_cloud_session_stop(
    app: tauri::AppHandle,
) -> Result<SessionSnapshot, String> {
    Ok(manager(&app)?.stop().await)
}

#[tauri::command]
pub fn cloudcraft_cloud_quota_status(
    app: tauri::AppHandle,
) -> Result<QuotaSnapshot, String> {
    Ok(manager(&app)?.quota_status())
}

#[tauri::command]
pub fn cloudcraft_cloud_quota_set_policy(
    app: tauri::AppHandle,
    policy: QuotaPolicy,
) -> Result<QuotaSnapshot, String> {
    manager(&app)?.set_quota_policy(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Copy)]
    enum StartBehavior {
        Succeed,
        SucceedAfterMs(u64),
        Fail,
        FailWithRemote,
        Hang,
    }

    struct FakeProvider {
        configured: AtomicBool,
        start_behavior: Mutex<StartBehavior>,
        stop_fails: AtomicBool,
        start_calls: AtomicUsize,
        stop_calls: AtomicUsize,
        stream_lost: AtomicBool,
        reconnect_fails: AtomicBool,
        reported_usage: Mutex<Option<u64>>,
    }

    impl FakeProvider {
        fn new(behavior: StartBehavior) -> Arc<Self> {
            Arc::new(Self {
                configured: AtomicBool::new(true),
                start_behavior: Mutex::new(behavior),
                stop_fails: AtomicBool::new(false),
                stream_lost: AtomicBool::new(false),
                reconnect_fails: AtomicBool::new(false),
                reported_usage: Mutex::new(None),
                start_calls: AtomicUsize::new(0),
                stop_calls: AtomicUsize::new(0),
            })
        }
    }

    impl CloudSessionProvider for FakeProvider {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn unavailable_reason(&self) -> Option<String> {
            (!self.configured.load(Ordering::SeqCst))
                .then(|| "Fake provider is off.".to_string())
        }

        fn start<'a>(
            &'a self,
            _request: &'a StartRequest,
        ) -> BoxFuture<'a, Result<RemoteSession, ProviderError>> {
            self.start_calls.fetch_add(1, Ordering::SeqCst);
            let behavior = *self.start_behavior.lock().unwrap();
            Box::pin(async move {
                match behavior {
                    StartBehavior::Succeed => Ok(RemoteSession {
                        remote_id: "remote-1".into(),
                    }),
                    StartBehavior::SucceedAfterMs(ms) => {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        Ok(RemoteSession {
                            remote_id: "remote-1".into(),
                        })
                    }
                    StartBehavior::Fail => Err(ProviderError {
                        message: "boom token=abc123".into(),
                        remote_id: None,
                    }),
                    StartBehavior::FailWithRemote => Err(ProviderError {
                        message: "half provisioned".into(),
                        remote_id: Some("remote-leaked".into()),
                    }),
                    StartBehavior::Hang => {
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                }
            })
        }

        fn stop<'a>(
            &'a self,
            _remote_id: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
            let fails = self.stop_fails.load(Ordering::SeqCst);
            Box::pin(async move {
                if fails {
                    Err("api down".into())
                } else {
                    Ok(())
                }
            })
        }

        fn health(&self, _remote_id: &str) -> Option<String> {
            self.stream_lost
                .load(Ordering::SeqCst)
                .then(|| "stream window closed".to_string())
        }

        fn supports_reconnect(&self) -> bool {
            true
        }

        fn reported_weekly_usage_secs(&self) -> Option<u64> {
            *self.reported_usage.lock().unwrap()
        }

        fn reconnect<'a>(
            &'a self,
            _remote_id: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            let fails = self.reconnect_fails.load(Ordering::SeqCst);
            if !fails {
                self.stream_lost.store(false, Ordering::SeqCst);
            }
            Box::pin(async move {
                if fails {
                    Err("host unreachable".into())
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn lost_stream_moves_ready_to_error_and_reconnect_restores_it() {
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = manager_with(provider.clone(), temp_store());
        manager.start(StartRequest::default()).await.unwrap();
        assert_eq!(manager.status().state, SessionState::Ready);
        assert!(!manager.status().can_reconnect);

        provider.stream_lost.store(true, Ordering::SeqCst);
        let lost = manager.status();
        assert_eq!(lost.state, SessionState::Error);
        assert!(lost.message.contains("stream window closed"));
        assert!(lost.can_reconnect && lost.can_stop && lost.cleanup_required);

        provider.reconnect_fails.store(true, Ordering::SeqCst);
        let failed = manager.reconnect().await.unwrap();
        assert_eq!(failed.state, SessionState::Error);
        assert!(
            failed.cleanup_required,
            "failed reconnect keeps the remote id"
        );

        provider.reconnect_fails.store(false, Ordering::SeqCst);
        let back = manager.reconnect().await.unwrap();
        assert_eq!(back.state, SessionState::Ready);

        let stopped = manager.stop().await;
        assert_eq!(stopped.state, SessionState::Stopped);
        assert!(
            manager.reconnect().await.is_err(),
            "nothing to reconnect after stop"
        );
    }

    fn temp_store() -> PathBuf {
        std::env::temp_dir()
            .join(format!("cloudcraft-session-test-{}", uuid::Uuid::new_v4()))
            .join("session.json")
    }

    fn quick() -> Timeouts {
        Timeouts {
            start: Duration::from_millis(200),
            stop: Duration::from_millis(200),
        }
    }

    fn manager_with(
        provider: Arc<FakeProvider>,
        store: PathBuf,
    ) -> Arc<SessionManager> {
        Arc::new(SessionManager::new(store, provider, quick()))
    }

    #[test]
    fn transition_table_rejects_illegal_moves() {
        use SessionState::*;
        assert!(Idle.can_transition_to(Starting));
        assert!(Starting.can_transition_to(Ready));
        assert!(!Idle.can_transition_to(Ready));
        assert!(!Stopped.can_transition_to(Ready));
        assert!(!Ready.can_transition_to(Starting));
        assert!(!Stopping.can_transition_to(Starting));
        assert!(!Unavailable.can_transition_to(Starting));
        assert!(!Ready.can_transition_to(Stopped));
    }

    #[test]
    fn set_state_returns_error_for_invalid_transition() {
        let manager = manager_with(
            FakeProvider::new(StartBehavior::Succeed),
            temp_store(),
        );
        let mut inner = manager.lock();
        assert!(inner.set_state(SessionState::Ready, "x").is_err());
        assert_eq!(inner.state, SessionState::Idle);
    }

    #[tokio::test]
    async fn unconfigured_provider_is_unavailable_and_never_starts() {
        let manager = SessionManager::new(
            temp_store(),
            Arc::new(UnconfiguredProvider),
            quick(),
        );
        let status = manager.status();
        assert_eq!(status.state, SessionState::Unavailable);
        assert!(!status.can_start && !status.can_stop);
        let error = manager.start(StartRequest::default()).await.unwrap_err();
        assert!(error.contains("No cloud gaming provider"));
        assert_eq!(manager.stop().await.state, SessionState::Unavailable);
    }

    #[tokio::test]
    async fn start_then_stop_releases_remote_session() {
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = manager_with(provider.clone(), temp_store());
        let ready = manager
            .start(StartRequest {
                game_version: Some("1.21.1".into()),
            })
            .await
            .unwrap();
        assert_eq!(ready.state, SessionState::Ready);
        assert!(ready.can_stop && !ready.can_start);
        let stopped = manager.stop().await;
        assert_eq!(stopped.state, SessionState::Stopped);
        assert!(!stopped.cleanup_required);
        assert_eq!(provider.stop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = manager_with(provider.clone(), temp_store());
        manager.start(StartRequest::default()).await.unwrap();
        manager.stop().await;
        manager.stop().await;
        manager.stop().await;
        assert_eq!(provider.stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(manager.status().state, SessionState::Stopped);
    }

    #[tokio::test]
    async fn duplicate_and_running_starts_are_rejected() {
        let provider = FakeProvider::new(StartBehavior::SucceedAfterMs(100));
        let manager = manager_with(provider.clone(), temp_store());
        let first = {
            let manager = manager.clone();
            tokio::spawn(
                async move { manager.start(StartRequest::default()).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = manager.start(StartRequest::default()).await.unwrap_err();
        assert!(second.contains("already starting"));
        assert_eq!(first.await.unwrap().unwrap().state, SessionState::Ready);
        let third = manager.start(StartRequest::default()).await.unwrap_err();
        assert!(third.contains("already running"));
        assert_eq!(provider.start_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_starts_provision_exactly_once() {
        let provider = FakeProvider::new(StartBehavior::SucceedAfterMs(50));
        let manager = manager_with(provider.clone(), temp_store());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let manager = manager.clone();
                tokio::spawn(async move {
                    manager.start(StartRequest::default()).await
                })
            })
            .collect();
        let mut successes = 0;
        for handle in handles {
            if handle.await.unwrap().is_ok() {
                successes += 1;
            }
        }
        assert_eq!(successes, 1);
        assert_eq!(provider.start_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancel_during_start_ends_stopped_without_remote_resource() {
        let provider = FakeProvider::new(StartBehavior::Hang);
        let manager = SessionManager::new(
            temp_store(),
            provider.clone(),
            Timeouts {
                start: Duration::from_secs(30),
                stop: Duration::from_secs(1),
            },
        );
        let manager = Arc::new(manager);
        let task = {
            let manager = manager.clone();
            tokio::spawn(
                async move { manager.start(StartRequest::default()).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(manager.status().can_cancel);
        manager.cancel_start();
        let result = task.await.unwrap().unwrap();
        assert_eq!(result.state, SessionState::Stopped);
        assert!(!result.cleanup_required);
        assert_eq!(provider.stop_calls.load(Ordering::SeqCst), 0);
        assert!(manager.cancel_start().state == SessionState::Stopped);
    }

    #[tokio::test]
    async fn cancel_after_provider_created_resource_releases_it() {
        let provider = FakeProvider::new(StartBehavior::SucceedAfterMs(60));
        let manager = manager_with(provider.clone(), temp_store());
        let task = {
            let manager = manager.clone();
            tokio::spawn(
                async move { manager.start(StartRequest::default()).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        // stop() during Starting is treated as a cancel request.
        manager.stop().await;
        let result = task.await.unwrap().unwrap();
        assert!(matches!(result.state, SessionState::Stopped));
    }

    #[tokio::test]
    async fn start_timeout_reports_error_and_allows_retry_without_remote_id() {
        let provider = FakeProvider::new(StartBehavior::Hang);
        let manager = manager_with(provider.clone(), temp_store());
        let result = manager.start(StartRequest::default()).await.unwrap();
        assert_eq!(result.state, SessionState::Error);
        assert!(result.message.contains("timed out"));
        assert!(result.can_start && !result.can_stop);
    }

    #[tokio::test]
    async fn failed_start_redacts_secrets_and_keeps_no_remote_id() {
        let provider = FakeProvider::new(StartBehavior::Fail);
        let manager = manager_with(provider, temp_store());
        let result = manager.start(StartRequest::default()).await.unwrap();
        assert_eq!(result.state, SessionState::Error);
        assert!(result.message.contains("token=[redacted]"));
        assert!(!result.message.contains("abc123"));
    }

    #[tokio::test]
    async fn failed_start_with_leaked_resource_requires_cleanup_before_restart()
    {
        let provider = FakeProvider::new(StartBehavior::FailWithRemote);
        let manager = manager_with(provider.clone(), temp_store());
        let result = manager.start(StartRequest::default()).await.unwrap();
        assert!(
            result.cleanup_required && result.can_stop && !result.can_start
        );
        assert!(manager.start(StartRequest::default()).await.is_err());
        let stopped = manager.stop().await;
        assert_eq!(stopped.state, SessionState::Stopped);
        assert!(!stopped.cleanup_required);
        assert_eq!(provider.stop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_stop_keeps_resource_and_can_be_retried() {
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = manager_with(provider.clone(), temp_store());
        manager.start(StartRequest::default()).await.unwrap();
        provider.stop_fails.store(true, Ordering::SeqCst);
        let failed = manager.stop().await;
        assert_eq!(failed.state, SessionState::Error);
        assert!(failed.cleanup_required && failed.can_stop);
        provider.stop_fails.store(false, Ordering::SeqCst);
        assert_eq!(manager.stop().await.state, SessionState::Stopped);
    }

    #[tokio::test]
    async fn interrupted_session_is_recovered_as_uncertain_and_cleanable() {
        let store = temp_store();
        let provider = FakeProvider::new(StartBehavior::Succeed);
        {
            let manager = manager_with(provider.clone(), store.clone());
            manager.start(StartRequest::default()).await.unwrap();
        }
        let restarted = manager_with(provider.clone(), store);
        let status = restarted.status();
        assert_eq!(status.state, SessionState::Error);
        assert!(status.cleanup_required && status.can_stop);
        assert!(status.message.contains("status is unknown"));
        assert_eq!(restarted.stop().await.state, SessionState::Stopped);
        assert_eq!(provider.stop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn interrupted_start_without_remote_id_recovers_as_stopped() {
        let store = temp_store();
        let provider = FakeProvider::new(StartBehavior::Hang);
        {
            let manager = Arc::new(SessionManager::new(
                store.clone(),
                provider.clone(),
                Timeouts {
                    start: Duration::from_secs(30),
                    stop: Duration::from_secs(1),
                },
            ));
            let task = {
                let manager = manager.clone();
                tokio::spawn(async move {
                    manager.start(StartRequest::default()).await
                })
            };
            tokio::time::sleep(Duration::from_millis(30)).await;
            task.abort();
        }
        let restarted = manager_with(provider, store);
        let status = restarted.status();
        assert_eq!(status.state, SessionState::Stopped);
        assert!(status.can_start);
    }

    #[tokio::test]
    async fn metadata_contains_no_credentials() {
        let store = temp_store();
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = manager_with(provider, store.clone());
        manager
            .start(StartRequest {
                game_version: Some("1.21.1".into()),
            })
            .await
            .unwrap();
        let saved = fs::read_to_string(&store).unwrap().to_lowercase();
        for forbidden in
            ["token", "password", "secret", "authorization", "bearer"]
        {
            assert!(
                !saved.contains(forbidden),
                "metadata contained {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn provider_becoming_available_moves_unavailable_to_idle() {
        let provider = FakeProvider::new(StartBehavior::Succeed);
        provider.configured.store(false, Ordering::SeqCst);
        let manager = manager_with(provider.clone(), temp_store());
        assert_eq!(manager.status().state, SessionState::Unavailable);
        provider.configured.store(true, Ordering::SeqCst);
        let status = manager.status();
        assert_eq!(status.state, SessionState::Idle);
        assert!(status.can_start);
    }

    #[tokio::test]
    async fn invalid_game_version_is_rejected_before_provisioning() {
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = manager_with(provider.clone(), temp_store());
        let error = manager
            .start(StartRequest {
                game_version: Some("1.21; rm -rf /".into()),
            })
            .await
            .unwrap_err();
        assert!(error.contains("unsupported characters"));
        assert_eq!(provider.start_calls.load(Ordering::SeqCst), 0);
    }

    use std::sync::atomic::AtomicI64;

    /// 2024-01-06 00:00:00 UTC, the start of a default quota week.
    const SAT: i64 = 1_704_499_200;
    const HOUR: i64 = 3600;

    fn clocked(
        provider: Arc<FakeProvider>,
        store: PathBuf,
        now: &Arc<AtomicI64>,
    ) -> SessionManager {
        let now = now.clone();
        SessionManager::with_clock(
            store,
            provider,
            quick(),
            Arc::new(move || now.load(Ordering::SeqCst)),
        )
    }

    #[tokio::test]
    async fn session_limit_stops_the_session_and_records_usage() {
        let now = Arc::new(AtomicI64::new(SAT));
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = clocked(provider.clone(), temp_store(), &now);
        let ready = manager.start(StartRequest::default()).await.unwrap();
        assert_eq!(ready.state, SessionState::Ready);
        assert_eq!(ready.quota.session_remaining_secs, Some(12 * 3600));

        now.store(SAT + 10 * HOUR, Ordering::SeqCst);
        let warnings = manager.tick().await;
        assert_eq!(warnings.len(), 1, "two-hour warning");
        assert_eq!(manager.status().state, SessionState::Ready);

        now.store(SAT + 12 * HOUR, Ordering::SeqCst);
        manager.tick().await;
        let stopped = manager.status();
        assert_eq!(stopped.state, SessionState::Stopped);
        assert!(stopped.message.contains("continuous-session limit"));
        assert_eq!(provider.stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(stopped.quota.weekly_used_secs, 12 * 3600);
        assert!(!stopped.quota.session_active);
        let end = stopped.quota.last_session_end.unwrap();
        assert_eq!(end.reason, EndReason::SessionLimit);
    }

    #[tokio::test]
    async fn exhausted_weekly_allowance_disables_play() {
        let now = Arc::new(AtomicI64::new(SAT));
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = clocked(provider.clone(), temp_store(), &now);
        for day in 0..3 {
            now.store(SAT + day * 24 * HOUR, Ordering::SeqCst);
            manager.start(StartRequest::default()).await.unwrap();
            now.store(SAT + day * 24 * HOUR + 10 * HOUR, Ordering::SeqCst);
            assert_eq!(manager.stop().await.state, SessionState::Stopped);
        }
        let status = manager.status();
        assert_eq!(status.quota.weekly_remaining_secs, 0);
        assert!(!status.can_start, "Play is disabled");
        let error = manager.start(StartRequest::default()).await.unwrap_err();
        assert!(error.contains("weekly cloud allowance"));
        assert_eq!(provider.start_calls.load(Ordering::SeqCst), 3);

        now.store(SAT + 7 * 24 * HOUR, Ordering::SeqCst);
        assert!(manager.status().can_start, "re-enabled after the reset");
    }

    #[tokio::test]
    async fn stop_twice_counts_usage_once() {
        let now = Arc::new(AtomicI64::new(SAT));
        let manager = clocked(
            FakeProvider::new(StartBehavior::Succeed),
            temp_store(),
            &now,
        );
        manager.start(StartRequest::default()).await.unwrap();
        now.store(SAT + HOUR, Ordering::SeqCst);
        manager.stop().await;
        now.store(SAT + 2 * HOUR, Ordering::SeqCst);
        manager.stop().await;
        assert_eq!(manager.status().quota.weekly_used_secs, 3600);
    }

    #[tokio::test]
    async fn failed_stop_keeps_counting_until_released() {
        let now = Arc::new(AtomicI64::new(SAT));
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = clocked(provider.clone(), temp_store(), &now);
        manager.start(StartRequest::default()).await.unwrap();
        provider.stop_fails.store(true, Ordering::SeqCst);
        now.store(SAT + HOUR, Ordering::SeqCst);
        assert_eq!(manager.stop().await.state, SessionState::Error);
        now.store(SAT + 3 * HOUR, Ordering::SeqCst);
        assert_eq!(manager.status().quota.weekly_used_secs, 3 * 3600);
        provider.stop_fails.store(false, Ordering::SeqCst);
        manager.stop().await;
        now.store(SAT + 5 * HOUR, Ordering::SeqCst);
        assert_eq!(manager.status().quota.weekly_used_secs, 3 * 3600);
    }

    #[tokio::test]
    async fn restart_with_live_remote_keeps_usage_and_relaunch_is_rechecked() {
        let now = Arc::new(AtomicI64::new(SAT));
        let store = temp_store();
        {
            let manager = clocked(
                FakeProvider::new(StartBehavior::Succeed),
                store.clone(),
                &now,
            );
            manager.start(StartRequest::default()).await.unwrap();
            now.store(SAT + HOUR, Ordering::SeqCst);
            manager.tick().await;
        }
        now.store(SAT + 4 * HOUR, Ordering::SeqCst);
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = clocked(provider.clone(), store, &now);
        let recovered = manager.status();
        assert_eq!(recovered.state, SessionState::Error);
        assert!(
            recovered.quota.session_active,
            "remote may be live: still counting"
        );
        assert_eq!(recovered.quota.session_elapsed_secs, Some(4 * 3600));
        assert!(!recovered.can_start);
        assert_eq!(manager.stop().await.state, SessionState::Stopped);
        let after = manager.status();
        assert_eq!(after.quota.weekly_used_secs, 4 * 3600);
        assert!(after.can_start);
    }

    #[tokio::test]
    async fn cancelled_and_failed_starts_close_the_counter() {
        let now = Arc::new(AtomicI64::new(SAT));
        let provider = FakeProvider::new(StartBehavior::Fail);
        let manager = clocked(provider.clone(), temp_store(), &now);
        manager.start(StartRequest::default()).await.unwrap();
        let status = manager.status();
        assert_eq!(status.state, SessionState::Error);
        assert!(!status.quota.session_active);
        assert!(status.can_start);
        assert_eq!(
            status.quota.last_session_end.unwrap().reason,
            EndReason::StartFailed
        );
    }

    #[tokio::test]
    async fn provider_reported_usage_is_authoritative() {
        let now = Arc::new(AtomicI64::new(SAT));
        let provider = FakeProvider::new(StartBehavior::Succeed);
        let manager = clocked(provider.clone(), temp_store(), &now);
        manager.start(StartRequest::default()).await.unwrap();
        assert_eq!(
            manager.status().quota.source,
            crate::cloudcraft_cloud_quota::UsageSource::LocalEstimate
        );
        *provider.reported_usage.lock().unwrap() = Some(29 * 3600);
        now.store(SAT + 60, Ordering::SeqCst);
        manager.tick().await;
        let status = manager.status();
        assert_eq!(
            status.quota.source,
            crate::cloudcraft_cloud_quota::UsageSource::Provider
        );
        assert_eq!(status.quota.weekly_used_secs, 29 * 3600);
        assert_eq!(
            status.quota.binding_limit,
            Some(crate::cloudcraft_cloud_quota::LimitKind::Weekly)
        );
    }

    #[test]
    fn redaction_removes_bearer_and_key_values() {
        let text = redact_secrets(
            "failed Bearer abc.def and api_key=xyz plus region=eu",
        );
        assert!(!text.contains("abc.def") && !text.contains("xyz"));
        assert!(text.contains("region=eu"));
    }
}
