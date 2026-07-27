//! Background FullEdge build telemetry and lifecycle state.
//!
//! Lives **outside** the main [`super::model::CodeGraph`] `RwLock` so HTTP handlers
//! can read status without contending on the graph. Extracted from `model.rs` (M2a)
//! so the graph model stays data-first.

/// Lifecycle of the background full edge build (workspace-scoped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum BackgroundEdgeBuildState {
    #[default]
    NotStarted = 0,
    Running = 1,
    Complete = 2,
    /// Build thread was signalled to stop (workspace switch) — may resume.
    Cancelled = 3,
    /// Partial edges on disk / in memory but build never finished.
    Incomplete = 4,
    /// Build thread panicked or exited abnormally (Sprint 8.2).
    Error = 5,
}

impl BackgroundEdgeBuildState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Running,
            2 => Self::Complete,
            3 => Self::Cancelled,
            4 => Self::Incomplete,
            5 => Self::Error,
            _ => Self::NotStarted,
        }
    }
}

/// Lock-free background edge-build telemetry (Sprint 6).
/// Lives outside the main [`CodeGraph`] `RwLock` so HTTP handlers can read status instantly.
///
/// **Honesty contract:** a FullEdge job must either advance (`beat` + phase), report
/// `blocked_reason`, or clear `thread_active` and leave Incomplete/Error — never silent
/// `live` with a truck of excuses.
#[derive(Debug)]
pub struct BgBuildProgress {
    pub files_processed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub files_total: std::sync::atomic::AtomicUsize,
    pub build_state: std::sync::atomic::AtomicU8,
    pub thread_active: std::sync::atomic::AtomicBool,
    /// Unix seconds of last file progress (FullEdge heartbeat). Detects zombie `live:true`.
    pub last_heartbeat_unix: std::sync::atomic::AtomicU64,
    /// When this job claimed the lane (`mark_job_started`). Used if beats never land.
    pub job_started_unix: std::sync::atomic::AtomicU64,
    /// Times idle reaper / request path re-enqueued after Incomplete.
    pub resume_count: std::sync::atomic::AtomicUsize,
    /// Coarse phase label for health + logs (`inventory`, `write_wait:maps`, …).
    pub phase: std::sync::Mutex<String>,
    /// Why the worker is not making progress (write wait, etc.). Cleared on real work.
    pub blocked_reason: std::sync::Mutex<Option<String>>,
}

impl BgBuildProgress {
    pub fn new(files_total: usize) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            files_processed: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            files_total: std::sync::atomic::AtomicUsize::new(files_total),
            build_state: std::sync::atomic::AtomicU8::new(
                BackgroundEdgeBuildState::NotStarted as u8,
            ),
            thread_active: std::sync::atomic::AtomicBool::new(false),
            last_heartbeat_unix: std::sync::atomic::AtomicU64::new(0),
            job_started_unix: std::sync::atomic::AtomicU64::new(0),
            resume_count: std::sync::atomic::AtomicUsize::new(0),
            phase: std::sync::Mutex::new(String::new()),
            blocked_reason: std::sync::Mutex::new(None),
        })
    }

    fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Claim the lane (enqueue or FullEdge start). Arms stale detection even before first beat.
    pub fn mark_job_started(&self) {
        use std::sync::atomic::Ordering;
        let now = Self::now_unix();
        self.job_started_unix.store(now, Ordering::Relaxed);
        self.last_heartbeat_unix.store(now, Ordering::Relaxed);
        self.thread_active.store(true, Ordering::Relaxed);
        self.clear_blocked();
        self.set_phase("queued");
    }

    /// Touch heartbeat (call on each file processed / batch start / write-wait tick).
    pub fn beat(&self) {
        use std::sync::atomic::Ordering;
        self.last_heartbeat_unix
            .store(Self::now_unix(), Ordering::Relaxed);
    }

    pub fn set_phase(&self, phase: impl Into<String>) {
        if let Ok(mut p) = self.phase.lock() {
            *p = phase.into();
        }
    }

    pub fn phase_str(&self) -> String {
        self.phase
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    pub fn set_blocked(&self, reason: impl Into<String>) {
        if let Ok(mut b) = self.blocked_reason.lock() {
            *b = Some(reason.into());
        }
    }

    pub fn clear_blocked(&self) {
        if let Ok(mut b) = self.blocked_reason.lock() {
            *b = None;
        }
    }

    pub fn blocked_str(&self) -> Option<String> {
        self.blocked_reason
            .lock()
            .ok()
            .and_then(|b| b.clone())
    }

    /// Seconds since last heartbeat (or job start). `None` if never started.
    pub fn heartbeat_age_secs(&self) -> Option<u64> {
        use std::sync::atomic::Ordering;
        let last = self.last_heartbeat_unix.load(Ordering::Relaxed);
        let started = self.job_started_unix.load(Ordering::Relaxed);
        let anchor = if last > 0 { last } else { started };
        if anchor == 0 {
            return None;
        }
        Some(Self::now_unix().saturating_sub(anchor))
    }

    /// True when no progress for `stale_secs` while claiming active.
    /// Uses `job_started_unix` when beats never landed (old bug: last==0 → never stale).
    pub fn heartbeat_stale(&self, stale_secs: u64) -> bool {
        use std::sync::atomic::Ordering;
        if !self.thread_active.load(Ordering::Relaxed) {
            return false;
        }
        let last = self.last_heartbeat_unix.load(Ordering::Relaxed);
        let started = self.job_started_unix.load(Ordering::Relaxed);
        let anchor = if last > 0 {
            last
        } else if started > 0 {
            started
        } else {
            return false;
        };
        let now = Self::now_unix();
        now.saturating_sub(anchor) > stale_secs
    }

    /// If FullEdge went silent (hang/OOM thrash), clear zombie live telemetry.
    pub fn clear_if_heartbeat_stale(&self, stale_secs: u64) -> bool {
        use std::sync::atomic::Ordering;
        if !self.heartbeat_stale(stale_secs) {
            return false;
        }
        let phase = self.phase_str();
        let blocked = self.blocked_str().unwrap_or_default();
        eprintln!(
            "⚠️  FullEdge heartbeat stale (>{}s) phase={} blocked={:?} — clearing zombie (processed={}/{})",
            stale_secs,
            phase,
            if blocked.is_empty() { "none" } else { blocked.as_str() },
            self.files_processed.load(Ordering::Relaxed),
            self.files_total.load(Ordering::Relaxed)
        );
        self.thread_active.store(false, Ordering::Relaxed);
        self.set_blocked(format!("heartbeat_stale>{stale_secs}s phase={phase}"));
        // Any non-terminal claim becomes Incomplete so idle reaper can resume.
        if !matches!(
            self.state(),
            BackgroundEdgeBuildState::Complete | BackgroundEdgeBuildState::Error
        ) {
            self.set_state(BackgroundEdgeBuildState::Incomplete);
        }
        self.set_phase("zombie_cleared");
        true
    }

    pub fn percent(&self) -> usize {
        use std::sync::atomic::Ordering;
        let total = self.files_total.load(Ordering::Relaxed);
        let done = self.files_processed.load(Ordering::Relaxed);
        if total == 0 {
            0
        } else if done == 0 {
            0
        } else if done >= total {
            100
        } else {
            // Floor at 1% once any work landed — torch sat at 0% for minutes while
            // integer truncation hid 40/6676-style progress on /mcp/health.
            (((done as u64 * 100) / total as u64).max(1)).min(99) as usize
        }
    }

    pub fn state(&self) -> BackgroundEdgeBuildState {
        BackgroundEdgeBuildState::from_u8(
            self.build_state.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    pub fn set_state(&self, state: BackgroundEdgeBuildState) {
        self.build_state
            .store(state as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// True when clients should see live edge progress.
    /// Heartbeat must be fresh (default **600s**) so long leviathan merges don't look dead;
    /// merge path also ticks heartbeat every 20s. Pre-first-beat zombies use job_started.
    pub fn is_live_build(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.clear_if_heartbeat_stale(600);
        self.thread_active.load(Ordering::Relaxed)
            && self.state() == BackgroundEdgeBuildState::Running
    }

    /// Authoritative post-complete transition: `Complete` state, 100% progress, thread cleared.
    pub fn mark_fully_complete(&self, files_done: usize) {
        use std::sync::atomic::Ordering;
        let total = self.files_total.load(Ordering::Relaxed);
        if files_done > 0 && total < files_done {
            self.files_total.store(files_done, Ordering::Relaxed);
        }
        let final_total = self.files_total.load(Ordering::Relaxed).max(files_done);
        self.files_processed.store(final_total, Ordering::Relaxed);
        self.set_state(BackgroundEdgeBuildState::Complete);
        self.thread_active.store(false, Ordering::Relaxed);
        self.clear_blocked();
        self.set_phase("complete");
        self.beat();
    }

    /// Terminal failure — thread cleared so HTTP handlers stop polling a dead worker.
    pub fn mark_failed(&self, files_done: usize) {
        use std::sync::atomic::Ordering;
        if files_done > 0 {
            self.files_processed.store(files_done, Ordering::Relaxed);
        }
        self.set_state(BackgroundEdgeBuildState::Error);
        self.thread_active.store(false, Ordering::Relaxed);
        self.set_phase("error");
        self.beat();
    }

    /// Incomplete after abort / write budget — reaper may resume.
    pub fn mark_incomplete_idle(&self, reason: &str) {
        use std::sync::atomic::Ordering;
        self.set_state(BackgroundEdgeBuildState::Incomplete);
        self.thread_active.store(false, Ordering::Relaxed);
        self.set_blocked(reason);
        self.set_phase("incomplete_idle");
        self.beat();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_stale_uses_job_started_when_no_beats() {
        use std::sync::atomic::Ordering;
        let t = BgBuildProgress::new(10);
        // Old bug: thread_active without last_heartbeat never went stale.
        t.thread_active.store(true, Ordering::Relaxed);
        t.job_started_unix.store(
            BgBuildProgress::now_unix().saturating_sub(120),
            Ordering::Relaxed,
        );
        t.last_heartbeat_unix.store(0, Ordering::Relaxed);
        assert!(t.heartbeat_stale(90));
        assert!(t.clear_if_heartbeat_stale(90));
        assert!(!t.thread_active.load(Ordering::Relaxed));
        assert_eq!(t.state(), BackgroundEdgeBuildState::Incomplete);
        assert!(t.blocked_str().is_some());
    }
}
