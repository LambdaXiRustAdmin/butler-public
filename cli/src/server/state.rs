use std::collections::{HashMap, VecDeque};
use std::sync::{atomic::AtomicBool, Arc, Mutex};
use std::time::Instant;

use code_graph::snooper::BgBuildProgress;
use tokio::sync::RwLock;

use cli::config::ButlerSettings;

use super::query_cache::SharedQueryCache;

/// Shared application state for the HTTP server.
///
/// Contains two concurrent hash maps protected by `tokio::sync::RwLock`:
/// - [`graphs`](AppState::graphs): Cached code graphs keyed by project root path.
///   Graphs are loaded lazily on first request and persist in memory for subsequent requests.
/// - [`in_progress`](AppState::in_progress): Tracks projects currently being scanned,
///   allowing the server to return a friendly "building graph" message instead of errors.
#[derive(Clone)]
pub struct AppState {
    pub graphs: Arc<RwLock<HashMap<String, Arc<std::sync::RwLock<code_graph::CodeGraph>>>>>,
    /// Tracks projects that are currently being scanned for the first time.
    /// Allows us to return a friendly "please wait, building graph" response with a timer
    /// instead of making clients hang for a long time.
    pub in_progress: Arc<RwLock<HashMap<String, BuildProgress>>>,
    /// Per-project cancellation tokens for the background full edge build task.
    /// Critical for true cancellation (tokio abort does not stop rayon/spawn_blocking).
    /// When loading a (new) workspace we signal previous bg tasks for other roots to stop.
    pub edge_build_cancels: Arc<RwLock<HashMap<String, Arc<AtomicBool>>>>,
    /// Per-project lock-free edge-build telemetry (Sprint 6 — outside CodeGraph RwLock).
    pub edge_build_status: Arc<RwLock<HashMap<String, Arc<BgBuildProgress>>>>,
    /// Loaded configuration (layered: defaults + global + workspace + env)
    pub settings: ButlerSettings,
    /// Whether the orchestrator has been used in this session.
    /// Starts as true if agent.expert_mode is enabled in config.
    pub orchestrator_has_run: Arc<AtomicBool>,
    /// Composed `/context` response cache (graph.version + prompt keyed).
    pub query_cache: SharedQueryCache,
    /// LRU order of graph roots for warm-set eviction (most recent at back).
    pub graph_lru: Arc<Mutex<VecDeque<String>>>,
    /// Hop B: last access time per root (for idle sleep). Updated on touch.
    pub graph_last_touch: Arc<Mutex<HashMap<String, Instant>>>,
}

/// Tracks the progress of an ongoing project graph scan.
///
/// Stored in [`AppState::in_progress`] from first request until the graph is fully loaded.
/// Clients polling `/context` receive this information to show elapsed time and current file.
#[derive(Clone, Debug)]
pub struct BuildProgress {
    pub start_time: Instant,
    /// Shared with the scanner so it can report the file currently being parsed.
    pub current_file: Option<Arc<Mutex<Option<String>>>>,
}
