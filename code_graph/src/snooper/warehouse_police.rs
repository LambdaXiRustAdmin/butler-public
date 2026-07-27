//! WarehousePolice — per-root mutation lanes + global FullEdge concurrency governor.
//!
//! **Why:** Tokio request threads, rayon edge pools, and post-passes fought over
//! `RwLock<CodeGraph>` write. EvePolice lesson: one lifecycle owner **per exclusive
//! resource**. The exclusive resource is a **single root's CodeGraph**, not the whole process.
//!
//! | Scope | Serialization |
//! |-------|----------------|
//! | **Within root** | One lane thread — FullEdge + JIT for that graph only |
//! | **Across roots** | Lanes run in parallel (fmt flesh ≠ gecko JIT wait) |
//! | **Global FullEdge** | At most N concurrent FullEdges (default 2) — mem governor |
//!
//! **Jobs:** `FullEdge` (background index), `JitFiles` (surgical ensure_call_graph).
//! HTTP must not hold the write lock across monorepo work — it enqueues and waits.

use super::builder;
use super::model::{BgBuildProgress, CodeGraph};

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Surgical JIT request — processed ASAP (including between full-edge batches).
struct JitRequest {
    graph: Arc<RwLock<CodeGraph>>,
    root: PathBuf,
    skips: Vec<String>,
    files: Vec<PathBuf>,
    /// Signaled when JIT finished (or failed soft).
    done: Sender<()>,
}

enum PoliceJob {
    /// Full background edge build for this root (serialized with other jobs on the same lane).
    FullEdge {
        graph: Arc<RwLock<CodeGraph>>,
        cancel: Arc<AtomicBool>,
        root: PathBuf,
        skips: Vec<String>,
        telemetry: Option<Arc<BgBuildProgress>>,
        edge_threads: usize,
    },
    /// Wake lane to drain pending JIT queue (when no FullEdge is running on this root).
    DrainJit,
}

/// One mutation lane for a single project root.
struct RootLane {
    job_tx: Sender<PoliceJob>,
    pending_jit: Arc<Mutex<VecDeque<JitRequest>>>,
}

/// Caps how many FullEdge jobs run process-wide (each may own a large rayon pool).
struct FullEdgeGovernor {
    max: usize,
    active: Mutex<usize>,
    cv: Condvar,
    /// Observability for health / logs.
    waiters: AtomicUsize,
}

impl FullEdgeGovernor {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max: max.max(1),
            active: Mutex::new(0),
            cv: Condvar::new(),
            waiters: AtomicUsize::new(0),
        })
    }

    fn max(&self) -> usize {
        self.max
    }

    fn active(&self) -> usize {
        *self.active.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Block until a FullEdge slot is free. While waiting, drain this root's JIT so
    /// interactive Trace is not parked behind a global slot queue.
    fn acquire_draining_jit(
        &self,
        root: &Path,
        telemetry: &Option<Arc<BgBuildProgress>>,
        pending_jit: &Mutex<VecDeque<JitRequest>>,
    ) {
        let mut guard = self.active.lock().unwrap_or_else(|p| p.into_inner());
        let t0 = Instant::now();
        let mut logged = false;
        while *guard >= self.max {
            if !logged {
                logged = true;
                self.waiters.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "🚧 FullEdge slot wait (active={}/{} waiters≈{}) root={} — draining JIT while waiting",
                    *guard,
                    self.max,
                    self.waiters.load(Ordering::Relaxed),
                    root.display()
                );
                if let Some(t) = telemetry {
                    t.set_phase("fulledge_slot_wait");
                    t.set_blocked(format!("fulledge_slots {}/{}", *guard, self.max));
                    t.beat();
                }
            }
            // Serve interactive JIT for this root while another root holds FullEdge slots.
            drop(guard);
            drain_pending_jit_budget(pending_jit, 8);
            if let Some(t) = telemetry {
                t.beat();
            }
            guard = self.active.lock().unwrap_or_else(|p| p.into_inner());
            let (g, _) = self
                .cv
                .wait_timeout(guard, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner());
            guard = g;
        }
        if logged {
            self.waiters.fetch_sub(1, Ordering::Relaxed);
            println!(
                "✅ FullEdge slot acquired after {:.1}s (active→{}/{}) root={}",
                t0.elapsed().as_secs_f64(),
                *guard + 1,
                self.max,
                root.display()
            );
            if let Some(t) = telemetry {
                t.clear_blocked();
                t.set_phase("fulledge_slot_acquired");
                t.beat();
            }
        }
        *guard += 1;
    }

    fn release(&self) {
        let mut guard = self.active.lock().unwrap_or_else(|p| p.into_inner());
        *guard = guard.saturating_sub(1);
        self.cv.notify_all();
    }
}

struct FullEdgeSlotGuard {
    gov: Arc<FullEdgeGovernor>,
}

impl Drop for FullEdgeSlotGuard {
    fn drop(&mut self) {
        self.gov.release();
    }
}

/// Process-wide warehouse traffic cop (per-root lanes).
pub struct WarehousePolice {
    lanes: Mutex<HashMap<String, RootLane>>,
    fulledge_slots: Arc<FullEdgeGovernor>,
}

static POLICE: OnceLock<WarehousePolice> = OnceLock::new();

/// Global police singleton (starts per-root workers on first job for that root).
pub fn warehouse_police() -> &'static WarehousePolice {
    POLICE.get_or_init(WarehousePolice::start)
}

fn root_key(root: &Path) -> String {
    // Stable lane id — trim trailing slash; keep container paths as-is.
    let s = root.to_string_lossy();
    s.trim_end_matches('/').to_string()
}

fn lane_thread_name(key: &str) -> String {
    let base = key.rsplit('/').next().unwrap_or("root");
    // Linux pthread name max ~15 bytes.
    let mut name = format!("wp-{base}");
    name.truncate(15);
    name
}

/// Max concurrent FullEdge jobs process-wide.
/// Override with `BUTLER_FULLEDGE_PARALLEL` (1–8). Default **2**.
fn fulledge_parallel_max() -> usize {
    std::env::var("BUTLER_FULLEDGE_PARALLEL")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.clamp(1, 8))
        .unwrap_or(2)
}

impl WarehousePolice {
    fn start() -> Self {
        let max = fulledge_parallel_max();
        println!(
            "🚓 WarehousePolice online (per-root lanes + FullEdge governor max={max})"
        );
        Self {
            lanes: Mutex::new(HashMap::new()),
            fulledge_slots: FullEdgeGovernor::new(max),
        }
    }

    /// Observability: (max_slots, active_fulledges, waiting_lanes_approx).
    pub fn fulledge_slot_status(&self) -> (usize, usize, usize) {
        (
            self.fulledge_slots.max(),
            self.fulledge_slots.active(),
            self.fulledge_slots.waiters.load(Ordering::Relaxed),
        )
    }

    fn lane_for(&self, root: &Path) -> RootLane {
        let key = root_key(root);
        let mut map = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(lane) = map.get(&key) {
            return RootLane {
                job_tx: lane.job_tx.clone(),
                pending_jit: Arc::clone(&lane.pending_jit),
            };
        }

        let (job_tx, job_rx) = mpsc::channel::<PoliceJob>();
        let pending_jit: Arc<Mutex<VecDeque<JitRequest>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let pending_worker = Arc::clone(&pending_jit);
        let slots = Arc::clone(&self.fulledge_slots);
        let key_for_thread = key.clone();
        let thread_name = lane_thread_name(&key);

        std::thread::Builder::new()
            .name(thread_name.clone())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                root_lane_loop(key_for_thread, job_rx, pending_worker, slots);
            })
            .unwrap_or_else(|e| panic!("WarehousePolice lane spawn failed: {e}"));

        println!(
            "🚓 WarehousePolice lane open: {} (thread={})",
            key, thread_name
        );

        let lane = RootLane {
            job_tx: job_tx.clone(),
            pending_jit: Arc::clone(&pending_jit),
        };
        map.insert(
            key,
            RootLane {
                job_tx,
                pending_jit,
            },
        );
        lane
    }

    /// Enqueue a full background edge build (non-blocking).
    /// Serialized with other FullEdge/JIT for **this root only**.
    pub fn submit_full_edge(
        &self,
        graph: Arc<RwLock<CodeGraph>>,
        cancel: Arc<AtomicBool>,
        root: PathBuf,
        skips: Vec<String>,
        telemetry: Option<Arc<BgBuildProgress>>,
        edge_threads: usize,
    ) {
        println!(
            "🚓 WarehousePolice enqueue FullEdge: {} (lane=per-root)",
            root.display()
        );
        if let Some(t) = telemetry.as_ref() {
            t.mark_job_started();
            t.set_phase("queued");
        }
        let lane = self.lane_for(&root);
        let _ = lane.job_tx.send(PoliceJob::FullEdge {
            graph,
            cancel,
            root,
            skips,
            telemetry,
            edge_threads,
        });
    }

    /// Surgical JIT for specific files. Blocks until done or `timeout`.
    /// Runs on **this root's** lane (or between its FullEdge batches).
    pub fn jit_files_blocking(
        &self,
        graph: Arc<RwLock<CodeGraph>>,
        root: PathBuf,
        skips: Vec<String>,
        files: Vec<PathBuf>,
        timeout: Duration,
    ) -> bool {
        if files.is_empty() {
            return true;
        }
        let lane = self.lane_for(&root);
        let (done_tx, done_rx) = mpsc::channel();
        {
            let mut q = lane
                .pending_jit
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            q.push_back(JitRequest {
                graph,
                root: root.clone(),
                skips,
                files,
                done: done_tx,
            });
        }
        // Wake idle lane (no-op if already in FullEdge — between_batches drains).
        let _ = lane.job_tx.send(PoliceJob::DrainJit);

        match done_rx.recv_timeout(timeout) {
            Ok(()) => true,
            Err(_) => {
                eprintln!(
                    "🚓 WarehousePolice JIT timeout ({timeout:?}) for {}",
                    root.display()
                );
                false
            }
        }
    }

    /// Enqueue surgical JIT and wait briefly. Used when FullEdge holds the write lock:
    /// next between-batch yield runs the job without parking multi-core collect forever.
    pub fn jit_files_yield_wait(
        &self,
        graph: Arc<RwLock<CodeGraph>>,
        root: PathBuf,
        skips: Vec<String>,
        files: Vec<PathBuf>,
        timeout: Duration,
    ) -> bool {
        self.jit_files_blocking(graph, root, skips, files, timeout)
    }
}

fn root_lane_loop(
    root_key: String,
    job_rx: Receiver<PoliceJob>,
    pending_jit: Arc<Mutex<VecDeque<JitRequest>>>,
    fulledge_slots: Arc<FullEdgeGovernor>,
) {
    loop {
        // Always clear interactive JIT first when idle between jobs.
        drain_pending_jit(&pending_jit);

        let job = match job_rx.recv() {
            Ok(j) => j,
            Err(_) => break,
        };

        match job {
            PoliceJob::DrainJit => {
                drain_pending_jit(&pending_jit);
            }
            PoliceJob::FullEdge {
                graph,
                cancel,
                root,
                skips,
                telemetry,
                edge_threads,
            } => {
                println!(
                    "🚓 WarehousePolice FullEdge start: {} (lane={} slots={}/{})",
                    root.display(),
                    root_key,
                    fulledge_slots.active(),
                    fulledge_slots.max()
                );
                if let Some(t) = telemetry.as_ref() {
                    t.mark_job_started();
                    t.set_phase("police_start");
                    t.set_state(crate::snooper::model::BackgroundEdgeBuildState::Running);
                }

                // Global mem governor — wait without starving this root's JIT.
                fulledge_slots.acquire_draining_jit(&root, &telemetry, &pending_jit);
                let _slot = FullEdgeSlotGuard {
                    gov: Arc::clone(&fulledge_slots),
                };

                // Drain interactive JIT **between** batches (after merge released the write
                // lock). Only this root's pending_jit — other roots have their own lanes.
                let pending = Arc::clone(&pending_jit);
                let between = move || {
                    drain_pending_jit_budget(&pending, 3);
                };
                let need_post = builder::run_background_full_edge_build_policed(
                    Arc::clone(&graph),
                    cancel,
                    root.clone(),
                    skips,
                    telemetry.clone(),
                    edge_threads,
                    Some(Box::new(between) as Box<dyn FnMut() + Send>),
                );
                // After inventory mapped: deferred PostPass (LTO map-reduce), yield to JIT.
                drain_pending_jit(&pending_jit);
                if need_post {
                    if let Some(t) = telemetry.as_ref() {
                        t.set_phase("post_pass");
                        t.beat();
                    }
                    let pending = Arc::clone(&pending_jit);
                    let between_post = move || {
                        drain_pending_jit(&pending);
                    };
                    builder::run_deferred_warehouse_post_pass_with_telemetry(
                        graph,
                        root.clone(),
                        telemetry,
                        Some(Box::new(between_post) as Box<dyn FnMut() + Send>),
                    );
                    drain_pending_jit(&pending_jit);
                    println!(
                        "🚓 WarehousePolice FullEdge+PostPass done: {}",
                        root.display()
                    );
                } else if let Some(t) = telemetry.as_ref() {
                    // Cancelled / write-budget abort / panic-handled — do **not** stamp Complete.
                    if t.thread_active.load(Ordering::Relaxed)
                        && matches!(
                            t.state(),
                            crate::snooper::model::BackgroundEdgeBuildState::Running
                        )
                    {
                        t.mark_incomplete_idle("fulledge_early_exit");
                    } else if t.thread_active.load(Ordering::Relaxed) {
                        t.thread_active.store(false, Ordering::Relaxed);
                    }
                    if let Ok(mut g) = graph.try_write() {
                        g.background_edge_build_active = false;
                        g.sync_bg_telemetry(Some(t.as_ref()));
                    }
                    println!(
                        "🚓 WarehousePolice FullEdge exit (no PostPass) phase={} blocked={:?} root={}",
                        t.phase_str(),
                        t.blocked_str(),
                        root.display()
                    );
                } else {
                    println!(
                        "🚓 WarehousePolice FullEdge exit (no PostPass) root={}",
                        root.display()
                    );
                }
                // _slot drops here → release global FullEdge capacity
            }
        }
    }
    println!("🚓 WarehousePolice lane exit: {root_key}");
}

fn drain_pending_jit(pending: &Mutex<VecDeque<JitRequest>>) {
    drain_pending_jit_budget(pending, usize::MAX);
}

/// Drain up to `max_jobs` pending surgical JIT requests (between FullEdge batches).
fn drain_pending_jit_budget(pending: &Mutex<VecDeque<JitRequest>>, max_jobs: usize) {
    for _ in 0..max_jobs {
        let next = {
            let mut q = pending.lock().unwrap_or_else(|p| p.into_inner());
            q.pop_front()
        };
        let Some(req) = next else {
            break;
        };
        run_jit(req);
    }
}

fn run_jit(req: JitRequest) {
    let n = req.files.len();
    println!(
        "🚓 WarehousePolice JIT: {} file(s) under {}",
        n,
        req.root.display()
    );
    {
        let mut g = req
            .graph
            .write()
            .unwrap_or_else(|p| p.into_inner());
        let _ = g.heal_false_edge_complete();
        if g.total_edges() == 0 {
            for p in &req.files {
                g.clear_file_edge_mark(p);
            }
        }
        g.ensure_call_graph(&req.root, &req.skips, Some(&req.files));
    }
    let _ = req.done.send(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_key_trims_slash() {
        assert_eq!(
            root_key(Path::new("/projects/foo/")),
            "/projects/foo"
        );
        assert_eq!(root_key(Path::new("/projects/foo")), "/projects/foo");
    }

    #[test]
    fn fulledge_governor_serializes_to_max() {
        let gov = FullEdgeGovernor::new(2);
        let empty = Mutex::new(VecDeque::new());
        let root = PathBuf::from("/t/a");
        gov.acquire_draining_jit(&root, &None, &empty);
        assert_eq!(gov.active(), 1);
        gov.acquire_draining_jit(&root, &None, &empty);
        assert_eq!(gov.active(), 2);
        gov.release();
        assert_eq!(gov.active(), 1);
        gov.release();
        assert_eq!(gov.active(), 0);
    }
}
