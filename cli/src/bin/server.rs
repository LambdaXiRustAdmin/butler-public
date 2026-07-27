//! HTTP server binary for Butler.
//!
//! Provides the main REST API (`/context`, `/projects`, etc.) consumed by LLM clients
//! and the MCP bridge. Uses a shared `CodeGraph` cache, offloads heavy work via
//! `spawn_blocking`, supports Docker path translation, and lazy surgical/line targeting.

use axum::{
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use socket2::{Domain, Protocol, Socket, Type};

use cli::config::ButlerSettings;

#[path = "../server/mod.rs"]
mod server;

use crate::server::context_engine::{
    collect_warm_roots, get_fingerprint, get_nickname, warehouse_idle_reaper_tick,
    warm_project_root,
};
use crate::server::discovery::*; // list_projects etc.
use crate::server::handlers::*; // brings mcp_*, handle_context etc.
use crate::server::query_cache;
use crate::server::state::*;

use cli::harvester::template::{
    Accuracy, Focus, Frontier, Incremental, Llm, Output, Polyglot, Template,
};
use cli::harvester::{agent_loop, llm as harv_llm, source::Source, tools::ToolRegistry};
use code_graph::{
    build_graph_export, build_graph_export_for_nodes, load_graph, write_graph_export, Id,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static HARVEST_CANCEL: AtomicBool = AtomicBool::new(false);
static HARVEST_RUNNING: AtomicBool = AtomicBool::new(false);
static HARVEST_LOGS: Mutex<Vec<String>> = Mutex::new(vec![]);

fn append_harvest_log(line: &str) {
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/harvester_live.log")
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{}\n", line).as_bytes()));
}

/// Entry point for the `server` binary.
///
/// Configures a Tokio runtime with a configurable stack size per worker thread (from
/// analysis.worker_stack_size_mb) to handle deep recursion in Tree-sitter parsing and graph
/// traversal safely.
///
/// The actual server bind uses SO_REUSEADDR + graceful SIGTERM shutdown (see async_main).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load settings early so we can use analysis.worker_stack_size_mb for the Tokio runtime.
    let settings = ButlerSettings::new();
    let stack_mb = settings.analysis.worker_stack_size_mb as usize;

    // Explicit worker thread count (Fix for Rayon + Tokio starvation under load in Docker).
    // Reserves headroom; rayon pool will use num_cpus-2. Use available_parallelism (respects cgroups)
    // or fall back to 8. This ensures Axum handlers keep responsive cores while bg edge builds run.
    let num_workers = std::thread::available_parallelism()
        .map(|p| p.get().max(2))
        .unwrap_or(8);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_workers)
        .enable_all()
        .thread_stack_size(stack_mb * 1024 * 1024)
        .build()?;

    runtime.block_on(async_main(settings))
}

/// Async main function that starts the Axum HTTP server.
///
/// Binds to a TCP listener on `{settings.server.host}:{settings.server.port}` using a manually
/// constructed `tokio::net::TcpListener` created from a `socket2::Socket` with `SO_REUSEADDR`
/// explicitly enabled. This allows the server (especially in Docker) to immediately re-bind
/// port 8002 even if a previous container left a phantom socket in TIME_WAIT.
///
/// Registers all route handlers and uses configuration from ButlerSettings (layered defaults +
/// global + workspace + env).
///
/// The server uses `with_graceful_shutdown` to handle SIGTERM (sent by `docker stop`) and Ctrl+C
/// for clean exit and port release.
async fn async_main(settings: ButlerSettings) -> Result<(), Box<dyn std::error::Error>> {
    let host = settings.server.host.clone();
    let port = settings.server.port;

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    // === Aggressive SO_REUSEADDR for Docker phantom ports ===
    // Docker stop kills the process but the host port mapping can leave TIME_WAIT sockets.
    // Setting SO_REUSEADDR *before* bind lets the next instance take the port immediately.
    // We use socket2 for a clean cross-platform way to configure the socket options prior to bind.
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;

    let std_listener = std::net::TcpListener::from(socket);
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    let pid = std::process::id();
    println!(
        "🚀 Butler server listening on http://{}:{} (PID: {})",
        host, port, pid
    );
    let startup_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    println!(
        "Fingerprint: {}",
        get_fingerprint(startup_root.to_string_lossy().as_ref())
    );
    let username = settings.server.username.trim();
    let username = if username.is_empty() {
        get_nickname(startup_root.to_string_lossy().as_ref())
    } else {
        username.to_string()
    };
    println!("Username:    {username}");
    let auth_on = settings
        .server
        .password
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if auth_on {
        println!("Auth:        password set (Bearer / Basic required on API routes)");
    } else {
        println!("Auth:        open (no server.password) — set BUTLER_PASSWORD for LAN/remote");
        if host == "0.0.0.0" || host == "::" {
            eprintln!(
                "⚠️  Bound on {host} with no password — anyone on the network can Trace/harvest your graphs.\n\
                 Prefer host=\"127.0.0.1\" for local-only, or set server.password / BUTLER_PASSWORD."
            );
        }
    }
    println!("Setup       → GET /setup          (first-run / proof of life)");
    println!("Operator    → GET /ops  (or /)    (export, harvest, full orchestrate)");
    println!("MCP ready   → GET /mcp/manifest   (for Claude Desktop, Cursor, etc.)");
    println!("Health      → GET /mcp/health");
    server::logv::log_boot_banner();
    if settings.agent.expert_mode {
        println!("Expert mode enabled via config.");
    }
    println!("Tip: config at ~/.config/butler/config.toml or .butler/config.toml — see plans/ALPHA_SETUP.md");

    let orchestrator_has_run = Arc::new(std::sync::atomic::AtomicBool::new(
        settings.agent.expert_mode,
    ));

    let query_cache_cap = settings.server.query_cache_cap;
    let app_state = AppState {
        graphs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        in_progress: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        edge_build_cancels: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        edge_build_status: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        settings: settings.clone(),
        orchestrator_has_run,
        query_cache: query_cache::new_shared(query_cache_cap),
        graph_lru: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
    };

    // P1: boot-warm configured roots (async load + watchers) so first /context is hot.
    // Must NOT run on the Tokio runtime thread — warm_project_root uses
    // tokio::sync::RwLock::blocking_* which panics inside async context.
    let warm_roots = collect_warm_roots(&settings);
    if !warm_roots.is_empty() {
        println!(
            "🔥 Warming {} root(s) at boot (background thread): {:?}",
            warm_roots.len(),
            warm_roots
        );
        let warm_state = app_state.clone();
        let roots = warm_roots;
        std::thread::Builder::new()
            .name("butler-warm-roots".into())
            .spawn(move || {
                for r in &roots {
                    warm_project_root(&warm_state, r);
                }
                println!("🔥 Boot warm registration complete ({} roots)", roots.len());
            })
            .expect("failed to spawn boot warm thread");
    } else {
        println!(
            "Tip: set BUTLER_WARM_ROOTS=/path1:/path2 or server.warm_roots in config to pre-load graphs."
        );
    }

    // Idle reaper: Incomplete FullEdge must finish when util is free — not thumb-twiddle at 4%.
    {
        let reaper_state = app_state.clone();
        std::thread::Builder::new()
            .name("butler-warehouse-reaper".into())
            .spawn(move || {
                println!(
                    "🧹 Warehouse idle reaper online (every 30s; resume Incomplete FullEdge)"
                );
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    warehouse_idle_reaper_tick(&reaper_state);
                }
            })
            .expect("failed to spawn warehouse idle reaper");
    }

    let app = Router::new()
        // Welcome / install (proof of life) — separate from operator lab.
        .route("/setup", get(server::setup_page::render_setup))
        .route("/welcome", get(server::setup_page::render_setup))
        // Operator lab (export, harvest, full orchestrate). `/` kept for dogfood bookmarks.
        .route("/", get(server::dashboard::render_dashboard))
        .route("/ops", get(server::dashboard::render_dashboard))
        .route("/dashboard", get(server::dashboard::render_dashboard))
        .route("/context", post(handle_context))
        .route("/warm", post(handle_warm))
        .route("/projects", get(list_projects))
        .route("/fingerprint", get(get_fingerprint_handler))
        // ── MCP endpoints (dynamic integration) ──
        .route("/mcp/manifest", get(mcp_manifest))
        .route("/mcp/health", get(mcp_health))
        .route("/collisions", post(handle_collisions))
        .route("/harvester", post(handle_harvester))
        .route("/harvester/cancel", post(cancel_harvester))
        .route("/harvester/status", get(harvester_status))
        .route("/export-graph", post(export_graph))
        .route("/build-training-bundle", post(build_training_bundle))
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            server::auth::auth_middleware,
        ))
        .with_state(app_state)
        .into_make_service();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Graceful shutdown hook for SIGTERM (Docker `docker stop`) and Ctrl+C.
///
/// Logs the shutdown message (as specified) so operators see the port is being released.
/// In-flight requests are allowed to complete by axum's graceful shutdown.
/// Any pending graph cache writes are the responsibility of the background watchers
/// (see code_graph::snooper::watcher); on clean process exit the OS closes fds and
/// any in-memory state is dropped. We log here for visibility.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    println!("Shutting down Butler server... releasing port.");
}

/// Basic harvester config handler for the web dashboard.
/// Accepts JSON with repo, llm_base, model, query, batch_size, max_steps, scope, ignore.
/// Fires the harvest in background (non-blocking) so /status polling can stream live logs.
/// Returns immediately with start confirmation.
async fn handle_harvester(
    axum::extract::Json(payload): axum::extract::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    use axum::http::StatusCode;
    use axum::Json as AxumJson;

    let repo = payload
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let llm_base = payload
        .get("llm_base")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:4000")
        .to_string();
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("grok-4.3")
        .to_string();
    let query = payload
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("core public API, traits, important impls, FFI boundaries")
        .to_string();
    let batch_size = payload
        .get("batch_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    let max_steps = payload
        .get("max_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    let scope_paths: Vec<String> = if let Some(s) = payload.get("scope").and_then(|v| v.as_str()) {
        s.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(arr) = payload.get("scope").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    } else {
        vec![]
    };

    let ignore_paths: Vec<String> = if let Some(s) = payload.get("ignore").and_then(|v| v.as_str())
    {
        s.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(arr) = payload.get("ignore").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    } else {
        vec![]
    };

    // Rich harvester controls (for training data on large graphs)
    let require_note = payload
        .get("require_exploration_note")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let require_rej = payload
        .get("require_explicit_rejections")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let require_edge = payload
        .get("require_reason_on_every_edge")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let min_neg = payload
        .get("min_hard_negatives_per_batch")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize;

    let use_ast = payload
        .get("use_ast_distance")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let use_deg = payload
        .get("use_degree")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let use_bm = payload
        .get("use_bm25")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let include_inter = payload
        .get("include_interconnect")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let load_prev = payload
        .get("load_previous_context")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let temperature = payload
        .get("temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.1) as f32;

    let api_key = payload
        .get("api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());

    let client_repo = repo.clone();
    // Translate host path (what the browser form sends) to container path if running under Docker.
    // Uses BUTLER_HOST_MOUNT / BUTLER_CONTAINER_MOUNT from docker-compose.
    let repo = server::paths::translate_client_path(&repo);

    let export_to_repo = payload
        .get("export_to_repo")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let export_repo = repo.clone();
    let export_dest: Option<String> = if export_to_repo {
        Some(
            std::path::Path::new(&export_repo)
                .join(".butler")
                .join("fat.json")
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };

    // Support pointing harvester at a specific graph_export (critical for training bundles).
    // E.g. ".butler/training/graph_export.json" or absolute in container view.
    // This makes node ids in fat match the exact export Eve training will use.
    let butler_export = payload
        .get("butler_export")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            if s.starts_with('/') {
                s.to_string()
            } else {
                std::path::Path::new(&repo)
                    .join(s)
                    .to_string_lossy()
                    .to_string()
            }
        });

    // Derive a repo-named output file so different web runs don't clobber each other.
    let safe_name: String = std::path::Path::new(&client_repo)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("harvester")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out_path = format!("/tmp/{}_fat.json", safe_name);
    let fat = PathBuf::from(&out_path);
    let resp_path = out_path.clone();

    // Respect "load previous context" when checked.
    // This is key for the training workflow:
    //   1. build-training-bundle writes a (rich) graph_export.json
    //   2. user points harvester at it (Graph export field) + checks "Load previous context"
    //
    // When load_prev:
    // - if we are exporting the fat to the repo, seed the temp fat from the
    //   existing repo .butler/fat.json (if any)
    // - run_harvest will then load the previous labels into state
    // - frontier will only offer unlabelled nodes from the graph_export
    // - the prompt will show "State: N nodes (C critical, R rejected)" so the LLM
    //   knows what has already been covered and extends the labeling intelligently.
    //
    // The final (extended) fat is still copied back at the end.
    if load_prev {
        if let Some(ref dest_str) = export_dest {
            let repo_fat = std::path::Path::new(dest_str);
            if repo_fat.exists() {
                let _ = std::fs::copy(repo_fat, &fat);
            }
        }
    } else {
        let _ = std::fs::remove_file(&fat);
    }

    let tpl = Template {
        name: "web-harvester".to_string(),
        query: query.clone(),
        repo: repo.clone(),
        butler_export: butler_export.clone(),
        output: Output {
            schema: "full_fat_v1".to_string(),
            format_version: 1,
        },
        incremental: Incremental {
            batch_size,
            max_steps,
            save_after_each: true,
            load_previous_context: load_prev,
            target_criticals: payload
                .get("target_criticals")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            target_rejections: payload
                .get("target_rejections")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
        },
        accuracy: Accuracy {
            require_exploration_note: require_note,
            require_reason_on_every_edge: require_edge,
            require_explicit_rejections: require_rej,
            min_hard_negatives_per_batch: min_neg,
            min_criticals_per_batch: 1,
            ban_stub_notes: true,
            require_label_polarity: true,
        },
        focus: Focus {
            scope_paths: scope_paths.clone(),
            ignore_paths: ignore_paths.clone(),
            prefer_high_degree: false,
        },
        frontier: Frontier {
            // Neighborhood cards: random walk + expand-from-critical (not degree hubs).
            strategy: "neighborhood".to_string(),
            use_ast_distance: use_ast,
            use_degree: use_deg,
            use_bm25: use_bm,
            card_profile: payload
                .get("card_profile")
                .and_then(|v| v.as_str())
                .unwrap_or("fast")
                .to_string(),
            max_neighbors: payload
                .get("max_neighbors")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            max_snippet_chars: payload
                .get("max_snippet_chars")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
        },
        llm: Llm {
            via: "litellm".to_string(),
            model: model.clone(),
            temperature,
        },
        polyglot: Polyglot {
            include_interconnect: include_inter,
        },
    };

    let src = Source::new(
        PathBuf::from(&repo),
        butler_export.as_ref().map(PathBuf::from),
    );
    let client = harv_llm::LlmClient::new(&llm_base, &model, api_key.as_deref());
    let reg = ToolRegistry::with_source(src.clone());

    HARVEST_CANCEL.store(false, Ordering::Relaxed);
    {
        if let Ok(mut logs) = HARVEST_LOGS.lock() {
            logs.clear();
        }
    }
    let _ = std::fs::write("/tmp/harvester_live.log", "");
    // Always emit a visible start line immediately so the very first status poll has something
    // (prevents the "Polling... [COMPLETE]" silent case when the run exits fast or before first LLM call).
    let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/harvester_live.log")
        .and_then(|mut f| {
            use std::io::Write;
            let key_info = if api_key.is_some() { "provided" } else { "from-env" };
            let export_info = if export_to_repo { "yes" } else { "no" };
            let be = butler_export.as_deref().unwrap_or("none");
            let acc = format!("note={} rej={} edge={} minneg={}", require_note, require_rej, require_edge, min_neg);
            let fr = format!("ast={} deg={} bm={}", use_ast, use_deg, use_bm);
            writeln!(f, "[harvester] === HARVEST START repo={} (internal={}) llm_base={} model={} query=\"{}\" batch_size={} max_steps={} scope={:?} ignore={:?} acc=({}) frontier=({}) poly={} loadprev={} temp={} api_key={} fat={} export_to_repo={} butler_export={}",
                client_repo, repo, llm_base, model, query, batch_size, max_steps, scope_paths, ignore_paths, acc, fr, include_inter, load_prev, temperature, key_info, out_path, export_info, be)
        });
    if let Some(ref p) = export_dest {
        append_harvest_log(&format!("[harvester] Export target: {}", p));
    }

    // Early validation: bad repo path is a common cause of "0 blocks, instant finish"
    let repo_p = std::path::Path::new(&repo);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("<unknown>"));
    let meta = std::fs::metadata(repo_p);
    let is_valid = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    if !is_valid {
        let reason = match &meta {
            Ok(m) if !m.is_dir() => "exists but is not a directory",
            Ok(_) => "exists but metadata check failed",
            Err(e) => match e.kind() {
                std::io::ErrorKind::NotFound => "no such file or directory",
                std::io::ErrorKind::PermissionDenied => "permission denied",
                _ => "other I/O error",
            },
        };
        let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/harvester_live.log")
            .and_then(|mut f| {
                use std::io::Write;
                let _ = writeln!(f, "[harvester] ERROR: repo path '{}' is invalid ({}). cwd={}", client_repo, reason, cwd.display());
                writeln!(f, "[harvester] HINT: The path is resolved by the *server process*. Use absolute path visible to it. Run `ls -ld '{}'` in the terminal where you started the server.", client_repo)
            });
        HARVEST_RUNNING.store(false, Ordering::Relaxed);
        // Return 200 so the dashboard JS treats it as a kickoff response (poll will show the ERROR details from logs).
        // This keeps the "start + watch status" flow even for immediate user input errors.
        return (
            StatusCode::OK,
            AxumJson(serde_json::json!({
                "started": false,
                "error": "invalid repo path",
                "repo": client_repo,
                "details": format!("{} (cwd={})", reason, cwd.display())
            })),
        );
    }

    HARVEST_RUNNING.store(true, Ordering::Relaxed);

    // Run harvest off the request handler so HTTP returns fast and /harvester/status can stream logs.
    let work = tokio::task::spawn_blocking({
        let export_to_repo = export_to_repo;
        let export_repo = export_repo;
        let fat = fat;
        let out_path = out_path;
        let safe_name = safe_name;
        move || {
            agent_loop::run_harvest(&tpl, &client, &reg, &src, &fat, Some(&HARVEST_CANCEL));
            if export_to_repo {
                let repo_dir = std::path::Path::new(&export_repo);
                match code_graph::snooper::ensure_project_butler_dir(repo_dir) {
                    Ok(butler_dir) => {
                        let dest = butler_dir.join("fat.json");
                        match std::fs::copy(&fat, &dest) {
                            Ok(_) => append_harvest_log(&format!(
                                "[harvester] Exported fat to {}",
                                dest.display()
                            )),
                            Err(e) => append_harvest_log(&format!(
                                "[harvester] Export copy FAILED to {}: {}",
                                dest.display(),
                                e
                            )),
                        }
                        // Also export a copy to the dataset folder (same place as _graph_export.json files)
                        // so fat + skinny exports live together for easy GNN pairing, no manual moves.
                        let dataset_dir = butler_dir.join("dataset");
                        let _ = std::fs::create_dir_all(&dataset_dir);
                        let dataset_dest = dataset_dir.join(format!("{}_fat.json", safe_name));
                        match std::fs::copy(&fat, &dataset_dest) {
                            Ok(_) => append_harvest_log(&format!(
                                "[harvester] Also exported fat to dataset: {}",
                                dataset_dest.display()
                            )),
                            Err(e) => append_harvest_log(&format!(
                                "[harvester] Dataset fat copy failed: {}",
                                e
                            )),
                        }
                    }
                    Err(e) => append_harvest_log(&format!(
                        "[harvester] skip fat export under {}: {e}",
                        repo_dir.display()
                    )),
                }
            }
            // Always keep the legacy path for the status UI and simple cp of "current" result.
            let _ = std::fs::copy(&fat, "/tmp/web_harvester_fat.json");
            // Also drop the "current" fat at the location Eve's load_fat_targets_auto looks for by default.
            // This way a fresh harvest "just works" for GNN gold labels without extra cp.
            let _ = std::fs::copy(&fat, "/tmp/fat_graph.json");
            // Record final summary into the live log so polling UI sees the result numbers.
            // Fat is now always saved by run_harvest (even empty runs), so we can report summary.
            // The detailed "Finished" line + start/loaded lines are already in live.log for the UI.
            if fat.exists() {
                if let Ok(data) = std::fs::read_to_string(&fat) {
                    if let Ok(g) = serde_json::from_str::<serde_json::Value>(&data) {
                        let n = g["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
                        let c = g["critical_node_ids"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let r = g["rejected_node_ids"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0);
                        return Some(
                            serde_json::json!({ "written": out_path, "nodes": n, "criticals": c, "rejections": r }),
                        );
                    }
                }
            }
            None
        }
    });
    // Detached waiter to clear running flag once blocking work ends (normal or cancelled).
    tokio::spawn(async move {
        let _ = work.await;
        HARVEST_RUNNING.store(false, Ordering::Relaxed);
    });

    (
        StatusCode::OK,
        AxumJson(
            serde_json::json!({ "started": true, "repo": repo, "output_path": resp_path, "export_path": export_dest }),
        ),
    )
}

async fn cancel_harvester() -> impl IntoResponse {
    HARVEST_CANCEL.store(true, Ordering::Relaxed);
    if let Ok(mut logs) = HARVEST_LOGS.lock() {
        logs.push("Cancellation requested".to_string());
    }
    axum::Json(serde_json::json!({"cancelled": true}))
}

async fn harvester_status() -> impl IntoResponse {
    let mut logs: Vec<String> = if let Ok(l) = HARVEST_LOGS.lock() {
        l.clone()
    } else {
        vec![]
    };
    // Also tail the live log file that the harvester writes to
    if let Ok(content) = std::fs::read_to_string("/tmp/harvester_live.log") {
        let lines: Vec<&str> = content.lines().collect();
        let tail = if lines.len() > 30 {
            &lines[lines.len() - 30..]
        } else {
            &lines[..]
        };
        for l in tail {
            logs.push(l.to_string());
        }
    }
    let is_running = HARVEST_RUNNING.load(Ordering::Relaxed);
    let fat_info = if std::path::Path::new("/tmp/web_harvester_fat.json").exists() {
        if let Ok(data) = std::fs::read_to_string("/tmp/web_harvester_fat.json") {
            if let Ok(g) = serde_json::from_str::<serde_json::Value>(&data) {
                let n = g["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
                Some(serde_json::json!({"nodes": n}))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    axum::Json(serde_json::json!({
        "running": is_running,
        "recent_logs": logs,
        "fat": fat_info
    }))
}

/// Force a fresh skinny graph export (graph_export.json) for a project.
/// Writes both to .butler/cache/ and .butler/dataset/<name>_graph_export.json
/// Useful from the dashboard to refresh the "skinny" export that pairs with harvester fat.
async fn export_graph(
    axum::extract::Json(payload): axum::extract::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    let project = payload
        .get("project")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let root = server::paths::translate_client_path(&project);

    // parse optional scope/ignore from form (for clean skinny exports)
    let scope_paths: Option<Vec<String>> = payload.get("scope_paths").and_then(|v| {
        if let Some(arr) = v.as_array() {
            let v: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        } else if let Some(s) = v.as_str() {
            let parts: Vec<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts)
            }
        } else {
            None
        }
    });
    let ignore_paths: Option<Vec<String>> = payload.get("ignore_paths").and_then(|v| {
        if let Some(arr) = v.as_array() {
            let v: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        } else if let Some(s) = v.as_str() {
            let parts: Vec<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts)
            }
        } else {
            None
        }
    });

    // Baseline strong ignores for clean "keepers" / GNN skinny exports.
    // Loaded from a file referenced in the wisperer repo (plans/keeper_baseline_ignores.txt).
    // This avoids duplicating the list in every test_repos/*/ .butlerignore .
    // User-provided ignores (from dashboard) are merged on top.
    let mut baseline_ignores: Vec<String> = vec![];
    if let Ok(content) = std::fs::read_to_string("plans/keeper_baseline_ignores.txt") {
        for line in content.lines() {
            let t = line.trim();
            if !t.is_empty() && !t.starts_with('#') {
                baseline_ignores.push(t.to_string());
            }
        }
    }
    if baseline_ignores.is_empty() {
        // fallback
        baseline_ignores = vec![
            "tests".to_string(),
            "test".to_string(),
            "benches".to_string(),
            "bench".to_string(),
            "examples".to_string(),
            "docs".to_string(),
            "tools".to_string(),
            "questions".to_string(),
            "imgs".to_string(),
            "assets".to_string(),
            ".github".to_string(),
            ".git".to_string(),
            ".faq".to_string(),
            "build".to_string(),
            "dist".to_string(),
            ".venv".to_string(),
            "__pycache__".to_string(),
            "node_modules".to_string(),
            "target".to_string(),
        ];
    }

    // Load (or force) the graph. Note: base scan still uses global skips + .butlerignore.
    // We filter at export time for the dataset skinny graph so keepers get clean data.
    let skip: Vec<String> = vec![];
    let graph = load_graph(&root, None, &skip);

    // Write standard cache export (full, for general Butler use)
    let _ = write_graph_export(&graph, std::path::Path::new(&root));

    // Write to dataset/ — filtered if scope/ignore provided (the one you copy to keepers)
    let dataset_dir = match code_graph::snooper::ensure_project_butler_dir(std::path::Path::new(&root))
    {
        Ok(b) => b.join("dataset"),
        Err(e) => {
            return axum::Json(serde_json::json!({
                "error": format!("refusing .butler write: {e}")
            }))
            .into_response();
        }
    };
    let _ = std::fs::create_dir_all(&dataset_dir);
    let name = std::path::Path::new(&project)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let out_path = dataset_dir.join(format!("{}_graph_export.json", name));

    let export = if scope_paths.is_some() || ignore_paths.is_some() {
        let scopes = scope_paths.clone().unwrap_or_default();
        let mut ignores = ignore_paths.clone().unwrap_or_default();
        // Always merge baseline (centralized here in wisperer export logic, mirroring HARVEST_GUIDE).
        // Long term: load from a file referenced in the wisperer repo (e.g. .butler/shared_ignores or plans/)
        // so test_repos don't need duplicated .butlerignore files.
        for b in &baseline_ignores {
            if !ignores.iter().any(|i| i == b) {
                ignores.push(b.clone());
            }
        }
        let mut matching: HashSet<Id> = HashSet::new();
        for (nid, _block) in &graph.nodes {
            let file_part = nid.as_str().split(':').next().unwrap_or(nid.as_str());
            let in_scope = scopes.is_empty()
                || scopes.iter().any(|s| {
                    let s = s.trim_end_matches('/');
                    file_part.contains(s) || file_part.starts_with(s)
                });
            // Safer ignore matching: look for /dir/ or dir/ segments to avoid "test" matching "test_repos"
            let not_ignored = ignores.is_empty()
                || !ignores.iter().any(|s| {
                    let s = s.trim_matches(|c: char| c == '/' || c == '.');
                    if s.is_empty() {
                        return false;
                    }
                    let as_dir = format!("/{}", s);
                    let as_dir2 = format!("{}/", s);
                    file_part.contains(&as_dir)
                        || file_part.contains(&as_dir2)
                        || file_part.split('/').any(|seg| seg == s)
                });
            if in_scope && not_ignored {
                matching.insert(nid.clone());
            }
        }
        println!("📤 scope+ignore (+baseline) filtered export: kept {}/{} nodes for dataset skinny graph", matching.len(), graph.nodes.len());
        build_graph_export_for_nodes(&graph, &matching, &HashMap::new(), false)
    } else {
        build_graph_export(&graph)
    };

    let json = serde_json::to_string_pretty(&export).unwrap_or_default();
    let _ = std::fs::write(&out_path, json);

    axum::Json(serde_json::json!({
        "success": true,
        "project": project,
        "cache_export": "written to .butler/cache/graph_export.json (full)",
        "dataset_export": out_path.to_string_lossy(),
        "filtered": scope_paths.is_some() || ignore_paths.is_some(),
        "kept_nodes": export.nodes.len()
    }))
    .into_response()
}

/// Build a training bundle (heavy graph export + synchronized fat labels) from the dashboard.
/// This is the dedicated path for Eve GNN training data (avoids side-effects in /context).
async fn build_training_bundle(
    axum::extract::Json(payload): axum::extract::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    let repo = payload
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let out_dir = payload
        .get("out_dir")
        .and_then(|v| v.as_str())
        .unwrap_or(".butler/training")
        .to_string();
    let _query = payload
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let repo_root = std::path::PathBuf::from(server::paths::translate_client_path(&repo));

    // Resolve output directory relative to the actual repo root so the bundle
    // appears at <repo>/.butler/training/ on the host (as requested).
    let bundle_dir: std::path::PathBuf = if out_dir.starts_with('/') {
        std::path::PathBuf::from(&out_dir)
    } else {
        repo_root.join(out_dir.trim_start_matches("./"))
    };
    if let Err(e) = std::fs::create_dir_all(&bundle_dir) {
        return axum::Json(serde_json::json!({ "error": format!("mkdir: {}", e) }));
    }

    let settings = ButlerSettings::new();
    let skips = &settings.analysis.skip_directories;

    append_harvest_log(&format!(
        "[bundle] === BUILD TRAINING BUNDLE repo={} out_dir={}",
        repo_root.display(),
        bundle_dir.display()
    ));

    // Training bundle always forces a fresh scan. This isolates the training data
    // generation from the normal skeleton cache: we want the current expanded
    // interesting_kinds (if/for/call/assign/return etc) + containment for WL, and
    // we don't want source-unrelated parser changes to be ignored due to hash match.
    // If this starts affecting "base" butler features undesirably we can further
    // split into a dedicated training-only structural exporter.
    let cache_bin = repo_root.join(".butler/cache/graph.bin");
    let _ = std::fs::remove_file(&cache_bin);

    let root_str = repo_root.to_string_lossy().to_string();
    let mut graph = load_graph(&root_str, None, skips);
    if graph.nodes.is_empty() {
        return axum::Json(serde_json::json!({ "error": "no nodes" }));
    }

    // For training data we want the *complete* materialized graph (all nodes + all
    // call/usage edges + containment). Normal butler defers edge building for fast
    // startup + on-demand/JIT for realtime searches/context. Training has none of
    // those constraints, so force a full synchronous build right now.
    graph.files_with_edges.clear();
    graph.background_edge_build_complete = false;
    graph.background_edge_build_active = false;
    append_harvest_log("[bundle] forcing FULL synchronous edge build (training only - no defer, no bg, no realtime)");

    // Heavy build for rich training data (calls + containment, expanded nodes from parser)
    graph.ensure_call_graph(&repo_root, skips, None);
    graph.compute_hubs(0.05);

    append_harvest_log(&format!(
        "[bundle] edge build complete: {} total edges in graph before export",
        graph.total_edges()
    ));

    // Structural export only (harvester is manual via the Harvester section
    // so you can control the LLM config, cost, and run it when ready).
    let export = build_graph_export_for_nodes(
        &graph,
        &graph.nodes.keys().cloned().collect(),
        &HashMap::new(),
        true,
    );
    let graph_path = bundle_dir.join("graph_export.json");
    let _ = std::fs::write(
        &graph_path,
        serde_json::to_string_pretty(&export).unwrap_or_default(),
    );
    append_harvest_log(&format!(
        "[bundle] exported {} nodes {} edges",
        export.nodes.len(),
        export.edges.len()
    ));

    let fat_path = bundle_dir.join("fat.json");

    axum::Json(serde_json::json!({
        "success": true,
        "repo": repo,
        "graph_export": graph_path.to_string_lossy(),
        "fat": fat_path.to_string_lossy(),
        "note": "Structural graph ready. Use the Harvester section (or CLI) manually with this graph_export as base to generate fat.json labels. This lets you control LLM, cost, etc."
    }))
}
