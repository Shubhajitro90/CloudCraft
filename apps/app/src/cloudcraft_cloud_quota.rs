//! Cloud gaming usage policy: a continuous-session limit and a weekly allowance.
//!
//! Two independent counters are kept in one persisted ledger:
//!
//! * **Counter A, continuous session:** time since the current session began.
//!   It is never split, paused, or reset by a reconnect or launcher restart.
//! * **Counter B, weekly allowance:** time used by every session inside the
//!   current quota week. Ending a session or starting another one never
//!   resets it; only the configured weekly reset boundary does.
//!
//! The defaults (12 h per session, 30 h per week, reset Saturday 00:00 UTC)
//! are CloudCraft's configured policy. They mirror figures Kaggle publishes
//! for its notebooks, but they are not provider data: unless a provider
//! reports usage through [`QuotaLedger::apply_provider_usage`], every figure
//! here is a local estimate from session start/stop times and is labelled so.
//!
//! All functions take `now` as Unix seconds so the policy is testable without
//! waiting and the session manager can inject its own clock.

use chrono::{Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_SESSION_SECS: u64 = 12 * 3600;
pub const DEFAULT_WEEKLY_ALLOWANCE_SECS: u64 = 30 * 3600;
/// Monday = 0 … Sunday = 6. Kaggle documents its weekly quota reset as Saturday 00:00 UTC.
pub const DEFAULT_RESET_WEEKDAY: u8 = 5;
/// A session shorter than this is not worth starting; Play stays disabled below it.
pub const MIN_LAUNCH_SECS: u64 = 60;

const WEEK_SECS: i64 = 7 * 24 * 3600;
const MAX_POLICY_SECS: u64 = 7 * 24 * 3600;
const MAX_THRESHOLDS: usize = 8;
const MAX_REMEMBERED: usize = 64;
const HEARTBEAT_PERSIST_SECS: i64 = 15;
const LEDGER_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaPolicy {
    pub max_session_secs: u64,
    pub weekly_allowance_secs: u64,
    pub reset_weekday: u8,
    pub reset_hour_utc: u8,
    /// Remaining-time thresholds for session warnings, e.g. 2 h, 30 min, 5 min.
    pub session_warning_secs: Vec<u64>,
    /// Remaining-allowance thresholds for weekly warnings (exhaustion always warns).
    pub weekly_warning_secs: Vec<u64>,
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self {
            max_session_secs: DEFAULT_MAX_SESSION_SECS,
            weekly_allowance_secs: DEFAULT_WEEKLY_ALLOWANCE_SECS,
            reset_weekday: DEFAULT_RESET_WEEKDAY,
            reset_hour_utc: 0,
            session_warning_secs: vec![2 * 3600, 30 * 60, 5 * 60],
            weekly_warning_secs: vec![2 * 3600, 30 * 60],
        }
    }
}

impl QuotaPolicy {
    /// Validates bounds and normalizes thresholds to unique, descending values.
    pub fn validated(mut self) -> Result<Self, String> {
        let in_range =
            |secs: u64| (MIN_LAUNCH_SECS..=MAX_POLICY_SECS).contains(&secs);
        if !in_range(self.max_session_secs) {
            return Err(
                "The session limit must be between 1 minute and 7 days.".into(),
            );
        }
        if !in_range(self.weekly_allowance_secs) {
            return Err(
                "The weekly allowance must be between 1 minute and 7 days."
                    .into(),
            );
        }
        if self.reset_weekday > 6 {
            return Err(
                "The reset weekday must be 0 (Monday) to 6 (Sunday).".into()
            );
        }
        if self.reset_hour_utc > 23 {
            return Err("The reset hour must be 0 to 23 (UTC).".into());
        }
        for list in [
            &mut self.session_warning_secs,
            &mut self.weekly_warning_secs,
        ] {
            if list.len() > MAX_THRESHOLDS
                || list.iter().any(|&t| t == 0 || t > MAX_POLICY_SECS)
            {
                return Err(format!(
                    "Warning thresholds must be 1 to {MAX_THRESHOLDS} values between 1 second and 7 days."
                ));
            }
            list.sort_unstable_by(|a, b| b.cmp(a));
            list.dedup();
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageSource {
    /// Measured by CloudCraft on this computer from session start/stop times.
    LocalEstimate,
    /// Reported by the compute provider.
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndReason {
    Stopped,
    SessionLimit,
    WeeklyAllowance,
    StartFailed,
    Cancelled,
    /// The launcher closed and the remote side was not confirmed alive afterwards.
    LauncherClosed,
    /// A newer session began while an older segment was still recorded as open.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WarningLevel {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitKind {
    Session,
    Weekly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWarning {
    /// Stable id, so a warning is announced once even across restarts.
    pub id: String,
    pub kind: LimitKind,
    pub level: WarningLevel,
    pub remaining_secs: u64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEndRecord {
    pub session_id: String,
    pub ended_at: i64,
    pub reason: EndReason,
    /// Seconds this session added to the current week's counter.
    pub counted_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastSessionEnd {
    pub session_id: String,
    pub ended_at: String,
    pub reason: EndReason,
    pub counted_secs: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSnapshot {
    pub policy: QuotaPolicy,
    pub source: UsageSource,
    pub source_note: String,
    pub session_active: bool,
    pub session_elapsed_secs: Option<u64>,
    pub session_remaining_secs: Option<u64>,
    pub session_deadline: Option<String>,
    /// Which limit ends the current session first.
    pub binding_limit: Option<LimitKind>,
    pub weekly_used_secs: u64,
    pub weekly_remaining_secs: u64,
    pub week_started_at: String,
    pub next_reset_at: String,
    pub can_launch: bool,
    pub block_reason: Option<String>,
    /// Warnings that apply right now, derived from tracked state.
    pub warnings: Vec<QuotaWarning>,
    pub last_session_end: Option<LastSessionEnd>,
    /// Set when the ledger had to be rebuilt; explains what happened.
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveSegment {
    session_id: String,
    started_at: i64,
    last_seen_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerData {
    version: u32,
    policy: QuotaPolicy,
    week_start: i64,
    weekly_used_secs: u64,
    source: UsageSource,
    active: Option<ActiveSegment>,
    counted_sessions: Vec<String>,
    fired_warnings: Vec<String>,
    last_end: Option<SessionEndRecord>,
    #[serde(default)]
    notice: Option<String>,
}

impl LedgerData {
    fn fresh(policy: QuotaPolicy, now: i64) -> Self {
        Self {
            version: LEDGER_VERSION,
            week_start: week_start_for(&policy, now),
            policy,
            weekly_used_secs: 0,
            source: UsageSource::LocalEstimate,
            active: None,
            counted_sessions: Vec::new(),
            fired_warnings: Vec::new(),
            last_end: None,
            notice: None,
        }
    }
}

/// Start of the quota week containing `now`.
pub fn week_start_for(policy: &QuotaPolicy, now: i64) -> i64 {
    let Some(moment) = Utc.timestamp_opt(now, 0).single() else {
        return now;
    };
    let weekday = i64::from(moment.weekday().num_days_from_monday());
    let days_back = (weekday - i64::from(policy.reset_weekday)).rem_euclid(7);
    let date = moment.date_naive() - chrono::Duration::days(days_back);
    let Some(naive) = date.and_hms_opt(u32::from(policy.reset_hour_utc), 0, 0)
    else {
        return now;
    };
    let candidate = Utc.from_utc_datetime(&naive).timestamp();
    if candidate > now {
        candidate - WEEK_SECS
    } else {
        candidate
    }
}

pub fn rfc3339(secs: i64) -> String {
    Utc.timestamp_opt(secs, 0)
        .single()
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

pub fn human_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    match (hours, minutes) {
        (0, 0) => format!("{secs} s"),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn level_for(threshold: u64) -> WarningLevel {
    if threshold <= 5 * 60 {
        WarningLevel::Critical
    } else if threshold <= 30 * 60 {
        WarningLevel::Warning
    } else {
        WarningLevel::Info
    }
}

fn end_message(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Stopped => "The cloud session was stopped.",
        EndReason::SessionLimit => "The cloud session reached its continuous-session limit and was stopped. CloudCraft cannot save a world inside the remote Minecraft client; progress not saved in-game before the limit may be lost.",
        EndReason::WeeklyAllowance => "The weekly cloud allowance ran out and the session was stopped. CloudCraft cannot save a world inside the remote Minecraft client; progress not saved in-game before the limit may be lost.",
        EndReason::StartFailed => "The cloud session failed to start. Time spent connecting was counted.",
        EndReason::Cancelled => "The cloud session start was cancelled. Time spent connecting was counted.",
        EndReason::LauncherClosed => "CloudCraft closed during the session. Usage was counted up to the last time the session was confirmed active.",
        EndReason::Superseded => "An older session record was closed at its last confirmed activity.",
    }
}

pub struct QuotaLedger {
    path: PathBuf,
    data: LedgerData,
    last_persisted_seen: i64,
}

impl QuotaLedger {
    /// Loads the ledger. An unreadable ledger is never treated as "no usage":
    /// it is kept aside and the week is treated as used until the next reset.
    pub fn load(path: PathBuf, now: i64) -> Self {
        let data = match fs::read_to_string(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                LedgerData::fresh(QuotaPolicy::default(), now)
            }
            Err(error) => Self::untrusted(&path, now, &error.to_string()),
            Ok(contents) => match serde_json::from_str::<LedgerData>(&contents)
            {
                Ok(data) if data.version == LEDGER_VERSION => {
                    match data.policy.clone().validated() {
                        Ok(policy) => LedgerData { policy, ..data },
                        Err(error) => Self::untrusted(&path, now, &error),
                    }
                }
                Ok(_) => {
                    Self::untrusted(&path, now, "unsupported ledger version")
                }
                Err(error) => Self::untrusted(&path, now, &error.to_string()),
            },
        };
        let mut ledger = Self {
            path,
            last_persisted_seen: data
                .active
                .as_ref()
                .map_or(0, |a| a.last_seen_at),
            data,
        };
        ledger.roll(now);
        ledger.persist();
        ledger
    }

    fn untrusted(path: &Path, now: i64, problem: &str) -> LedgerData {
        let kept = path.with_extension(format!("json.unreadable-{now}"));
        let kept_note = match fs::copy(path, &kept) {
            Ok(_) => {
                format!(" The unreadable file was kept as {}.", kept.display())
            }
            Err(_) => String::new(),
        };
        tracing::warn!("Cloud usage ledger could not be read: {problem}");
        let mut data = LedgerData::fresh(QuotaPolicy::default(), now);
        data.weekly_used_secs = data.policy.weekly_allowance_secs;
        data.notice = Some(format!(
            "CloudCraft could not read its cloud usage records, so this week's allowance is treated as used until the next reset.{kept_note}"
        ));
        data
    }

    fn persist(&mut self) {
        let write = || -> Result<(), String> {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let serialized = serde_json::to_string_pretty(&self.data)
                .map_err(|e| e.to_string())?;
            let temp = self.path.with_extension("json.tmp");
            fs::write(&temp, serialized).map_err(|e| e.to_string())?;
            fs::rename(&temp, &self.path).map_err(|e| e.to_string())
        };
        match write() {
            Ok(()) => {
                self.last_persisted_seen =
                    self.data.active.as_ref().map_or(0, |a| a.last_seen_at);
            }
            Err(error) => {
                tracing::warn!("Could not persist cloud usage ledger: {error}")
            }
        }
    }

    /// Advances to the quota week containing `now`. A clock set backwards never
    /// moves the week back and so cannot restore spent allowance.
    fn roll(&mut self, now: i64) {
        if now < self.data.week_start + WEEK_SECS {
            return;
        }
        let week_start = self.data.week_start;
        self.data.week_start = week_start_for(&self.data.policy, now);
        self.data.weekly_used_secs = 0;
        self.data.source = UsageSource::LocalEstimate;
        self.data.notice = None;
        let old_week = format!("week:{week_start}:");
        self.data
            .fired_warnings
            .retain(|id| !id.starts_with(&old_week));
    }

    fn active_weekly_portion(&self, now: i64) -> u64 {
        self.data.active.as_ref().map_or(0, |a| {
            (now - a.started_at.max(self.data.week_start)).max(0) as u64
        })
    }

    fn close(
        &mut self,
        session_id: &str,
        end_at: i64,
        reason: EndReason,
    ) -> Option<u64> {
        let active = self
            .data
            .active
            .as_ref()
            .filter(|a| a.session_id == session_id)?
            .clone();
        let end_at = end_at.max(active.started_at);
        let counted = (end_at - active.started_at.max(self.data.week_start))
            .max(0) as u64;
        self.data.weekly_used_secs =
            self.data.weekly_used_secs.saturating_add(counted);
        self.data.active = None;
        self.data.counted_sessions.push(session_id.to_string());
        if self.data.counted_sessions.len() > MAX_REMEMBERED {
            let excess = self.data.counted_sessions.len() - MAX_REMEMBERED;
            self.data.counted_sessions.drain(..excess);
        }
        let session_prefix = format!("session:{session_id}:");
        self.data
            .fired_warnings
            .retain(|id| !id.starts_with(&session_prefix));
        self.data.last_end = Some(SessionEndRecord {
            session_id: session_id.to_string(),
            ended_at: end_at,
            reason,
            counted_secs: counted,
        });
        Some(counted)
    }

    /// Starts counting a session. Idempotent for the session already counting;
    /// refuses a session whose usage was already recorded, and refuses to start
    /// when the weekly allowance cannot cover at least [`MIN_LAUNCH_SECS`].
    pub fn begin(&mut self, session_id: &str, now: i64) -> Result<(), String> {
        self.roll(now);
        if let Some(active) = self.data.active.clone() {
            if active.session_id == session_id {
                return Ok(());
            }
            self.close(
                &active.session_id,
                active.last_seen_at,
                EndReason::Superseded,
            );
        }
        if self.data.counted_sessions.iter().any(|id| id == session_id) {
            self.persist();
            return Err("This session's usage was already recorded. Start a new session.".into());
        }
        let snapshot = self.snapshot(now);
        if !snapshot.can_launch {
            self.persist();
            return Err(snapshot
                .block_reason
                .unwrap_or_else(|| "Cloud Play is not available.".into()));
        }
        self.data.active = Some(ActiveSegment {
            session_id: session_id.to_string(),
            started_at: now,
            last_seen_at: now,
        });
        self.persist();
        Ok(())
    }

    /// Records that the session was still active at `now`. Persisted at most
    /// every few seconds; this is the evidence used after an unclean exit.
    pub fn heartbeat(&mut self, now: i64) {
        self.roll(now);
        let Some(active) = self.data.active.as_mut() else {
            return;
        };
        if now > active.last_seen_at {
            active.last_seen_at = now;
        }
        if now - self.last_persisted_seen >= HEARTBEAT_PERSIST_SECS {
            self.persist();
        }
    }

    /// Stops counting. Returns the seconds added, or `None` for a duplicate or
    /// unknown event, which changes nothing.
    pub fn end(
        &mut self,
        session_id: &str,
        now: i64,
        reason: EndReason,
    ) -> Option<u64> {
        self.roll(now);
        let counted = self.close(session_id, now, reason);
        if counted.is_some() {
            self.persist();
        }
        counted
    }

    /// Called once at launcher start. If the remote side may still be running
    /// (the session manager kept a remote id) the session keeps counting until
    /// it is stopped. Otherwise it is closed at its last confirmed activity.
    pub fn recover(&mut self, remote_may_be_alive: bool, now: i64) {
        self.roll(now);
        if remote_may_be_alive {
            return;
        }
        if let Some(active) = self.data.active.clone() {
            self.close(
                &active.session_id,
                active.last_seen_at,
                EndReason::LauncherClosed,
            );
            self.persist();
        }
    }

    pub fn active_session_id(&self) -> Option<&str> {
        self.data.active.as_ref().map(|a| a.session_id.as_str())
    }

    /// `Some(reason)` once the running session has no time left under either limit.
    pub fn enforcement(&self, now: i64) -> Option<EndReason> {
        let snapshot = self.snapshot(now);
        if snapshot.session_active && snapshot.session_remaining_secs == Some(0)
        {
            return Some(match snapshot.binding_limit {
                Some(LimitKind::Weekly) => EndReason::WeeklyAllowance,
                _ => EndReason::SessionLimit,
            });
        }
        None
    }

    /// Replaces local weekly usage with an authoritative provider figure that
    /// includes the running session.
    pub fn apply_provider_usage(&mut self, used_this_week_secs: u64, now: i64) {
        self.roll(now);
        let running = self.active_weekly_portion(now);
        self.data.weekly_used_secs =
            used_this_week_secs.saturating_sub(running);
        self.data.source = UsageSource::Provider;
        self.persist();
    }

    /// Changes the policy. Refused while a session is counting; usage is kept.
    pub fn set_policy(
        &mut self,
        policy: QuotaPolicy,
        now: i64,
    ) -> Result<(), String> {
        let policy = policy.validated()?;
        if self.data.active.is_some() {
            return Err(
                "Stop the cloud session before changing usage limits.".into()
            );
        }
        self.roll(now);
        let reset_changed = policy.reset_weekday
            != self.data.policy.reset_weekday
            || policy.reset_hour_utc != self.data.policy.reset_hour_utc;
        self.data.policy = policy;
        if reset_changed {
            // The current usage is kept and carried into the week defined by the new boundary.
            self.data.week_start = week_start_for(&self.data.policy, now);
        }
        self.persist();
        Ok(())
    }

    fn current_warnings(&self, snapshot: &QuotaSnapshot) -> Vec<QuotaWarning> {
        let mut warnings = Vec::new();
        if let (Some(active), Some(remaining)) =
            (self.data.active.as_ref(), snapshot.session_remaining_secs)
        {
            let weekly_binds =
                snapshot.binding_limit == Some(LimitKind::Weekly);
            let limit_name = if weekly_binds {
                "weekly allowance"
            } else {
                "session limit"
            };
            let deadline =
                snapshot.session_deadline.clone().unwrap_or_default();
            if remaining == 0 {
                warnings.push(QuotaWarning {
                    id: format!("session:{}:0", active.session_id),
                    kind: LimitKind::Session,
                    level: WarningLevel::Critical,
                    remaining_secs: 0,
                    message: format!("The {limit_name} has been reached. CloudCraft is stopping the session."),
                });
            } else if let Some(&threshold) = self
                .data
                .policy
                .session_warning_secs
                .iter()
                .filter(|&&t| remaining <= t)
                .min()
            {
                warnings.push(QuotaWarning {
                    id: format!("session:{}:{threshold}", active.session_id),
                    kind: LimitKind::Session,
                    level: level_for(threshold),
                    remaining_secs: remaining,
                    message: format!(
                        "{} left before the {limit_name} ends this cloud session at {deadline}. Save your world in Minecraft now.",
                        human_duration(remaining)
                    ),
                });
            }
        }
        let week = self.data.week_start;
        let remaining = snapshot.weekly_remaining_secs;
        if remaining == 0 {
            warnings.push(QuotaWarning {
                id: format!("week:{week}:exhausted"),
                kind: LimitKind::Weekly,
                level: WarningLevel::Critical,
                remaining_secs: 0,
                message: format!(
                    "The weekly cloud allowance is used up ({} of {}). Cloud Play is disabled until {}.",
                    human_duration(snapshot.weekly_used_secs),
                    human_duration(self.data.policy.weekly_allowance_secs),
                    snapshot.next_reset_at
                ),
            });
        } else if let Some(&threshold) = self
            .data
            .policy
            .weekly_warning_secs
            .iter()
            .filter(|&&t| remaining <= t)
            .min()
        {
            warnings.push(QuotaWarning {
                id: format!("week:{week}:{threshold}"),
                kind: LimitKind::Weekly,
                level: level_for(threshold),
                remaining_secs: remaining,
                message: format!(
                    "{} of this week's cloud allowance left. It resets at {}.",
                    human_duration(remaining),
                    snapshot.next_reset_at
                ),
            });
        }
        warnings
    }

    /// Warnings not announced before. Each id is announced once (persisted), and
    /// crossing several thresholds at once announces only the most urgent one.
    pub fn take_new_warnings(&mut self, now: i64) -> Vec<QuotaWarning> {
        self.roll(now);
        let snapshot = self.snapshot(now);
        let mut fresh = Vec::new();
        for warning in snapshot.warnings {
            if self.data.fired_warnings.contains(&warning.id) {
                continue;
            }
            let prefix = warning
                .id
                .rsplit_once(':')
                .map_or("", |(p, _)| p)
                .to_string();
            let thresholds = match warning.kind {
                LimitKind::Session => &self.data.policy.session_warning_secs,
                LimitKind::Weekly => &self.data.policy.weekly_warning_secs,
            };
            let superseded: Vec<String> = thresholds
                .iter()
                .filter(|&&t| t >= warning.remaining_secs)
                .map(|t| format!("{prefix}:{t}"))
                .collect();
            for id in superseded
                .into_iter()
                .chain(std::iter::once(warning.id.clone()))
            {
                if !self.data.fired_warnings.contains(&id) {
                    self.data.fired_warnings.push(id);
                }
            }
            fresh.push(warning);
        }
        if !fresh.is_empty() {
            if self.data.fired_warnings.len() > MAX_REMEMBERED {
                let excess = self.data.fired_warnings.len() - MAX_REMEMBERED;
                self.data.fired_warnings.drain(..excess);
            }
            self.persist();
        }
        fresh
    }

    pub fn snapshot(&self, now: i64) -> QuotaSnapshot {
        let policy = &self.data.policy;
        // Usage is never computed against a week that has already ended.
        let (week_start, committed, source) =
            if now >= self.data.week_start + WEEK_SECS {
                (week_start_for(policy, now), 0, UsageSource::LocalEstimate)
            } else {
                (
                    self.data.week_start,
                    self.data.weekly_used_secs,
                    self.data.source,
                )
            };
        let running =
            self.data.active.as_ref().map_or(0, |a| {
                (now - a.started_at.max(week_start)).max(0) as u64
            });
        let weekly_used = committed.saturating_add(running);
        let weekly_remaining =
            policy.weekly_allowance_secs.saturating_sub(weekly_used);
        let next_reset = week_start + WEEK_SECS;

        let (elapsed, session_remaining, deadline, binding) =
            match self.data.active.as_ref() {
                Some(active) => {
                    let elapsed = (now - active.started_at).max(0) as u64;
                    let by_session =
                        policy.max_session_secs.saturating_sub(elapsed);
                    let binding = if weekly_remaining < by_session {
                        LimitKind::Weekly
                    } else {
                        LimitKind::Session
                    };
                    let remaining = by_session.min(weekly_remaining);
                    (
                        Some(elapsed),
                        Some(remaining),
                        Some(rfc3339(now + remaining as i64)),
                        Some(binding),
                    )
                }
                None => (None, None, None, None),
            };

        let block_reason = if self.data.active.is_some() {
            Some("A cloud session is already being counted. Stop it before starting another.".to_string())
        } else if weekly_remaining < MIN_LAUNCH_SECS {
            Some(format!(
                "The weekly cloud allowance has less than 1 minute left ({} of {} used). Cloud Play is available again after the configured reset at {}.",
                human_duration(weekly_used),
                human_duration(policy.weekly_allowance_secs),
                rfc3339(next_reset)
            ))
        } else {
            None
        };

        let mut snapshot = QuotaSnapshot {
            policy: policy.clone(),
            source,
            source_note: match source {
                UsageSource::LocalEstimate => "Estimated by CloudCraft from session start and stop times on this computer. This is not provider data.".into(),
                UsageSource::Provider => "Reported by the compute provider.".into(),
            },
            session_active: self.data.active.is_some(),
            session_elapsed_secs: elapsed,
            session_remaining_secs: session_remaining,
            session_deadline: deadline,
            binding_limit: binding,
            weekly_used_secs: weekly_used,
            weekly_remaining_secs: weekly_remaining,
            week_started_at: rfc3339(week_start),
            next_reset_at: rfc3339(next_reset),
            can_launch: block_reason.is_none(),
            block_reason,
            warnings: Vec::new(),
            last_session_end: self.data.last_end.as_ref().map(|end| LastSessionEnd {
                session_id: end.session_id.clone(),
                ended_at: rfc3339(end.ended_at),
                reason: end.reason,
                counted_secs: end.counted_secs,
                message: end_message(end.reason).to_string(),
            }),
            notice: self.data.notice.clone(),
        };
        snapshot.warnings = self.current_warnings(&snapshot);
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    /// 2024-01-06 00:00:00 UTC, a Saturday.
    const SAT: i64 = 1_704_499_200;

    fn path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("cloudcraft-quota-test-{}", uuid::Uuid::new_v4()))
            .join("quota.json")
    }

    #[test]
    fn week_starts_on_saturday_midnight_utc() {
        let policy = QuotaPolicy::default();
        assert_eq!(week_start_for(&policy, SAT), SAT);
        assert_eq!(week_start_for(&policy, SAT - 1), SAT - 7 * 24 * H);
        assert_eq!(week_start_for(&policy, SAT + 4 * 24 * H + 12 * H), SAT);
        assert_eq!(week_start_for(&policy, SAT + 7 * 24 * H), SAT + 7 * 24 * H);
        let start = Utc
            .timestamp_opt(week_start_for(&policy, 1_800_000_000), 0)
            .unwrap();
        assert_eq!(start.weekday(), chrono::Weekday::Sat);
        assert_eq!(start.timestamp() % 86_400, 0);
    }

    #[test]
    fn continuous_session_limit_is_enforced_at_twelve_hours() {
        let mut ledger = QuotaLedger::load(path(), SAT + H);
        ledger.begin("s1", SAT + H).unwrap();
        assert_eq!(ledger.enforcement(SAT + 13 * H - 1), None);
        assert_eq!(
            ledger.enforcement(SAT + 13 * H),
            Some(EndReason::SessionLimit)
        );
        let snap = ledger.snapshot(SAT + 13 * H);
        assert_eq!(snap.session_remaining_secs, Some(0));
        assert_eq!(snap.binding_limit, Some(LimitKind::Session));
        assert_eq!(
            ledger.end("s1", SAT + 13 * H, EndReason::SessionLimit),
            Some(12 * 3600)
        );
        // Ending the session does not reset the weekly counter.
        assert_eq!(ledger.snapshot(SAT + 13 * H).weekly_used_secs, 12 * 3600);
    }

    #[test]
    fn weekly_usage_accumulates_across_sessions_and_disables_play() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        for (i, start) in
            [SAT, SAT + 24 * H, SAT + 48 * H].into_iter().enumerate()
        {
            let id = format!("s{i}");
            ledger.begin(&id, start).unwrap();
            ledger.end(&id, start + 10 * H, EndReason::Stopped).unwrap();
        }
        let snap = ledger.snapshot(SAT + 60 * H);
        assert_eq!(snap.weekly_used_secs, 30 * 3600);
        assert_eq!(snap.weekly_remaining_secs, 0);
        assert!(!snap.can_launch);
        assert!(snap
            .block_reason
            .unwrap()
            .contains(&rfc3339(SAT + 7 * 24 * H)));
        assert!(ledger.begin("s9", SAT + 61 * H).is_err());
        assert!(snap.warnings.iter().any(|w| w.id.ends_with(":exhausted")));
    }

    #[test]
    fn weekly_allowance_can_end_a_session_before_twelve_hours() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("a", SAT).unwrap();
        ledger.end("a", SAT + 25 * H, EndReason::Stopped).unwrap();
        ledger.begin("b", SAT + 30 * H).unwrap();
        let snap = ledger.snapshot(SAT + 30 * H);
        assert_eq!(snap.session_remaining_secs, Some(5 * 3600));
        assert_eq!(snap.binding_limit, Some(LimitKind::Weekly));
        assert_eq!(
            ledger.enforcement(SAT + 35 * H),
            Some(EndReason::WeeklyAllowance)
        );
    }

    #[test]
    fn warnings_fire_once_per_threshold_and_skip_stale_ones() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("s", SAT).unwrap();
        assert!(ledger.take_new_warnings(SAT + H).is_empty());

        let two_hours = ledger.take_new_warnings(SAT + 10 * H);
        assert_eq!(two_hours.len(), 1);
        assert_eq!(two_hours[0].level, WarningLevel::Info);
        assert!(
            ledger.take_new_warnings(SAT + 10 * H + 60).is_empty(),
            "no duplicate"
        );

        let thirty = ledger.take_new_warnings(SAT + 11 * H + 30 * 60);
        assert_eq!(thirty.len(), 1);
        assert_eq!(thirty[0].level, WarningLevel::Warning);

        let five = ledger.take_new_warnings(SAT + 12 * H - 5 * 60);
        assert_eq!(five.len(), 1);
        assert_eq!(five[0].level, WarningLevel::Critical);

        let mut jumped = QuotaLedger::load(path(), SAT);
        jumped.begin("j", SAT).unwrap();
        let only_urgent = jumped.take_new_warnings(SAT + 12 * H - 60);
        assert_eq!(
            only_urgent.len(),
            1,
            "crossing three thresholds at once announces one"
        );
        assert_eq!(only_urgent[0].level, WarningLevel::Critical);
        assert!(jumped.take_new_warnings(SAT + 12 * H - 30).is_empty());
    }

    #[test]
    fn weekly_warnings_and_exhaustion_are_announced() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("a", SAT).unwrap();
        ledger
            .end("a", SAT + 28 * H + 30 * 60, EndReason::Stopped)
            .unwrap();
        let approaching = ledger.take_new_warnings(SAT + 29 * H);
        assert_eq!(approaching.len(), 1);
        assert_eq!(approaching[0].kind, LimitKind::Weekly);
        ledger.begin("b", SAT + 29 * H).unwrap();
        ledger.end("b", SAT + 31 * H, EndReason::Stopped).unwrap();
        let exhausted = ledger.take_new_warnings(SAT + 31 * H);
        assert!(exhausted.iter().any(|w| w.id.ends_with(":exhausted")));
        assert!(ledger.take_new_warnings(SAT + 32 * H).is_empty());
    }

    #[test]
    fn fired_warnings_survive_restart() {
        let store = path();
        let mut ledger = QuotaLedger::load(store.clone(), SAT);
        ledger.begin("s", SAT).unwrap();
        assert_eq!(ledger.take_new_warnings(SAT + 10 * H).len(), 1);
        drop(ledger);
        let mut reloaded = QuotaLedger::load(store, SAT + 10 * H + 5);
        reloaded.recover(true, SAT + 10 * H + 5);
        assert!(reloaded.take_new_warnings(SAT + 10 * H + 10).is_empty());
    }

    #[test]
    fn restart_keeps_counting_when_remote_may_be_alive() {
        let store = path();
        let mut ledger = QuotaLedger::load(store.clone(), SAT);
        ledger.begin("s", SAT).unwrap();
        ledger.heartbeat(SAT + H);
        drop(ledger);
        let mut reloaded = QuotaLedger::load(store, SAT + 3 * H);
        reloaded.recover(true, SAT + 3 * H);
        assert_eq!(reloaded.active_session_id(), Some("s"));
        assert_eq!(
            reloaded.snapshot(SAT + 3 * H).session_elapsed_secs,
            Some(3 * 3600)
        );
        assert_eq!(
            reloaded.end("s", SAT + 4 * H, EndReason::Stopped),
            Some(4 * 3600)
        );
    }

    #[test]
    fn unexpected_termination_counts_until_last_confirmed_activity() {
        let store = path();
        let mut ledger = QuotaLedger::load(store.clone(), SAT);
        ledger.begin("s", SAT).unwrap();
        ledger.heartbeat(SAT + 2 * H);
        drop(ledger);
        let mut reloaded = QuotaLedger::load(store.clone(), SAT + 9 * H);
        reloaded.recover(false, SAT + 9 * H);
        let snap = reloaded.snapshot(SAT + 9 * H);
        assert!(!snap.session_active);
        assert_eq!(snap.weekly_used_secs, 2 * 3600);
        assert_eq!(
            snap.last_session_end.unwrap().reason,
            EndReason::LauncherClosed
        );
        // A second restart does not count the same session again.
        drop(reloaded);
        let again = QuotaLedger::load(store, SAT + 10 * H);
        assert_eq!(again.snapshot(SAT + 10 * H).weekly_used_secs, 2 * 3600);
    }

    #[test]
    fn duplicate_usage_events_are_ignored() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("s", SAT).unwrap();
        ledger.begin("s", SAT + H).unwrap();
        assert_eq!(
            ledger.snapshot(SAT + H).session_elapsed_secs,
            Some(3600),
            "re-begin does not restart the clock"
        );
        assert_eq!(
            ledger.end("s", SAT + 2 * H, EndReason::Stopped),
            Some(2 * 3600)
        );
        assert_eq!(ledger.end("s", SAT + 3 * H, EndReason::Stopped), None);
        assert_eq!(
            ledger.end("unknown", SAT + 3 * H, EndReason::Stopped),
            None
        );
        assert!(
            ledger.begin("s", SAT + 3 * H).is_err(),
            "a counted session cannot be reopened"
        );
        assert_eq!(ledger.snapshot(SAT + 3 * H).weekly_used_secs, 2 * 3600);
    }

    #[test]
    fn weekly_reset_restores_allowance_and_splits_a_spanning_session() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("a", SAT).unwrap();
        ledger.end("a", SAT + 29 * H, EndReason::Stopped).unwrap();
        let next = SAT + 7 * 24 * H;
        ledger.begin("b", next - H).unwrap();
        assert_eq!(ledger.snapshot(next - 1).weekly_used_secs, 30 * 3600 - 1);
        let after = ledger.snapshot(next + 2 * H);
        assert_eq!(
            after.weekly_used_secs,
            2 * 3600,
            "only the part after the reset counts in the new week"
        );
        assert_eq!(
            after.session_elapsed_secs,
            Some(3 * 3600),
            "the continuous counter is not split"
        );
        assert_eq!(
            ledger.end("b", next + 2 * H, EndReason::Stopped),
            Some(2 * 3600)
        );
        assert!(ledger.snapshot(next + 2 * H).can_launch);
    }

    #[test]
    fn clock_moved_backwards_does_not_restore_allowance() {
        let mut ledger = QuotaLedger::load(path(), SAT + 24 * H);
        ledger.begin("a", SAT + 24 * H).unwrap();
        ledger.end("a", SAT + 34 * H, EndReason::Stopped).unwrap();
        let snap = ledger.snapshot(SAT - 3 * 24 * H);
        assert_eq!(snap.weekly_used_secs, 10 * 3600);
    }

    #[test]
    fn provider_usage_replaces_the_local_estimate() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("a", SAT).unwrap();
        ledger.end("a", SAT + 2 * H, EndReason::Stopped).unwrap();
        ledger.begin("b", SAT + 3 * H).unwrap();
        ledger.apply_provider_usage(10 * 3600, SAT + 4 * H);
        let snap = ledger.snapshot(SAT + 4 * H);
        assert_eq!(snap.source, UsageSource::Provider);
        assert_eq!(
            snap.weekly_used_secs,
            10 * 3600,
            "provider figure includes the running session"
        );
        assert_eq!(ledger.snapshot(SAT + 5 * H).weekly_used_secs, 11 * 3600);
        ledger.end("b", SAT + 5 * H, EndReason::Stopped);
        assert_eq!(ledger.snapshot(SAT + 5 * H).weekly_used_secs, 11 * 3600);
        // A new week returns to local estimation.
        assert_eq!(
            ledger.snapshot(SAT + 8 * 24 * H).source,
            UsageSource::LocalEstimate
        );
        ledger.heartbeat(SAT + 8 * 24 * H);
        assert_eq!(ledger.snapshot(SAT + 8 * 24 * H).weekly_used_secs, 0);
    }

    #[test]
    fn unreadable_ledger_is_treated_as_used_not_reset() {
        let store = path();
        fs::create_dir_all(store.parent().unwrap()).unwrap();
        fs::write(&store, "{ not json").unwrap();
        let ledger = QuotaLedger::load(store.clone(), SAT + H);
        let snap = ledger.snapshot(SAT + H);
        assert!(!snap.can_launch);
        assert!(snap.notice.unwrap().contains("could not read"));
        let kept = fs::read_dir(store.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("unreadable"));
        assert!(kept, "the unreadable file is preserved");
        let next_week = QuotaLedger::load(store, SAT + 7 * 24 * H + H);
        assert!(next_week.snapshot(SAT + 7 * 24 * H + H).can_launch);
    }

    #[test]
    fn policy_changes_are_validated_and_keep_usage() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("a", SAT).unwrap();
        assert!(
            ledger.set_policy(QuotaPolicy::default(), SAT + H).is_err(),
            "refused while counting"
        );
        ledger.end("a", SAT + 5 * H, EndReason::Stopped).unwrap();
        let bad = QuotaPolicy {
            max_session_secs: 0,
            ..QuotaPolicy::default()
        };
        assert!(ledger.set_policy(bad, SAT + 5 * H).is_err());
        let bad_day = QuotaPolicy {
            reset_weekday: 9,
            ..QuotaPolicy::default()
        };
        assert!(ledger.set_policy(bad_day, SAT + 5 * H).is_err());
        let tighter = QuotaPolicy {
            max_session_secs: 4 * 3600,
            weekly_allowance_secs: 6 * 3600,
            session_warning_secs: vec![300, 1800, 300],
            ..QuotaPolicy::default()
        };
        ledger.set_policy(tighter, SAT + 5 * H).unwrap();
        let snap = ledger.snapshot(SAT + 5 * H);
        assert_eq!(snap.weekly_used_secs, 5 * 3600);
        assert_eq!(snap.weekly_remaining_secs, 3600);
        assert_eq!(snap.policy.session_warning_secs, vec![1800, 300]);
    }

    #[test]
    fn short_remaining_allowance_blocks_launch() {
        let mut ledger = QuotaLedger::load(path(), SAT);
        ledger.begin("a", SAT).unwrap();
        ledger
            .end("a", SAT + 30 * H - 30, EndReason::Stopped)
            .unwrap();
        let snap = ledger.snapshot(SAT + 30 * H);
        assert_eq!(snap.weekly_remaining_secs, 30);
        assert!(!snap.can_launch);
    }
}
