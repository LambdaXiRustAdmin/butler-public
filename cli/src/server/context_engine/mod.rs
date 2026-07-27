//! Context engine: request handling for `/context` and MCP tool dispatch.
//!
//! # Request stages (see `plans/context-request-lifecycle.md`)
//! 1. Ingress / normalize ask → goal+mode ([`mode_intent`], `normalize_butler_ask_request`)
//! 2. Resolve project root + monorepo scope heal
//! 3. Graph admit / load / BUILDING / skeleton serve
//! 4. Effective mode ([`mode_intent::compute_effective_mode`]) + orchestrate vs select_blocks
//! 5. Orchestrate Trace/Arch/Find **or** compose context
//! 6. Response assembly (content + structured)
//!
//! Goal/mode string parsing is **not** scattered ad-hoc — use
//! [`crate::server::mode_intent`] (Pack A).
//!
//! **P3 peels:** [`resolve`] · [`graph_admit`] · [`building`] · [`surgical`] · [`dispatch`].
//! **P3.1 stages:** [`ingress`] · project gate · [`load_lobby`] · [`front_door`] ·
//! [`serve_prep`] · [`surgical_phase`] · [`compose_path`].

mod resolve;
mod graph_admit;
mod building;
mod surgical;
mod dispatch;
mod front_door;
mod ingress;
mod load_lobby;
mod serve_prep;
mod surgical_phase;
mod compose_path;

use resolve::{try_project_gate, ProjectGateOutcome, ProjectGateReady};
use front_door::{try_front_door, FrontDoorContinue, FrontDoorOutcome};
use ingress::{run_ingress, IngressOutcome, IngressReady};
use load_lobby::{try_load_lobby, LoadLobbyOutcome, LoadLobbyReady};
use serve_prep::{try_serve_prep, ServePrepOutcome};
use surgical_phase::{run_surgical_phase, SurgicalPhaseOutcome};
use compose_path::run_compose_path;

pub use graph_admit::{collect_warm_roots, warm_project_root, warehouse_idle_reaper_tick};

use axum::{http::StatusCode, Json};
use std::time::Instant;

use crate::server::dto::*;
use crate::server::state::*;
use code_graph::NeuralSelectionBlend;

pub use ingress::effective_prompt_for_request;

// (MCP schemas used in thin handlers.rs surface, not here)

// --- statics (support for engine + thin surface) ---
// Concurrency guard: limits concurrent heavy requests (graph load + ensure + Phase 4 + composition)
// so a greedy LLM pile-up cannot starve Tokio / rayon. Default **4**; raise for load tests via
// `BUTLER_QUERY_PARALLEL` (1–512). Permit held only for spawn_blocking work.
pub(crate) static QUERY_SEMAPHORE: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| {
        let n = std::env::var("BUTLER_QUERY_PARALLEL")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|n| n.clamp(1, 512))
            .unwrap_or(4);
        println!("🚦 QUERY_SEMAPHORE permits={n} (BUTLER_QUERY_PARALLEL)");
        tokio::sync::Semaphore::new(n)
    });

// --- Support fns (pub for thin handlers to delegate if needed; small) ---
// Create `.butler/` only under allowed project roots (refuse nested src/examples/tests).
fn ensure_butler_dir(root: &std::path::Path) -> Option<std::path::PathBuf> {
    match code_graph::snooper::ensure_project_butler_dir(root) {
        Ok(dir) => Some(dir),
        Err(e) => {
            eprintln!(
                "⚠️  skip .butler under {}: {e}",
                root.display()
            );
            None
        }
    }
}

pub fn get_fingerprint(root: &str) -> String {
    let root_path = std::path::Path::new(root);
    let Some(butler_dir) = ensure_butler_dir(root_path) else {
        // Nested/forbidden root: ephemeral id (do not litter the tree).
        return format!("butler-ephemeral-{}", uuid::Uuid::new_v4().simple());
    };
    let fp_path = butler_dir.join("fingerprint");

    if let Ok(existing) = std::fs::read_to_string(&fp_path) {
        existing.trim().to_string()
    } else {
        let fp = format!("butler-{}", uuid::Uuid::new_v4().simple());
        let _ = std::fs::write(&fp_path, &fp);
        fp
    }
}

pub fn get_nickname(root: &str) -> String {
    // Prefer layered server config (hostname by default).
    let from_settings = cli::config::ButlerSettings::new()
        .server
        .username
        .trim()
        .to_string();
    if !from_settings.is_empty() {
        return from_settings;
    }

    let root_path = std::path::Path::new(root);
    let Some(butler_dir) = ensure_butler_dir(root_path) else {
        return cli::config::default_server_username();
    };
    let config_path = butler_dir.join("config.toml");

    if let Ok(content) = std::fs::read_to_string(&config_path) {
        for line in content.lines() {
            let t = line.trim();
            if let Some(rest) = t
                .strip_prefix("nickname=")
                .or_else(|| t.strip_prefix("nickname ="))
                .or_else(|| t.strip_prefix("username="))
                .or_else(|| t.strip_prefix("username ="))
            {
                let n = rest.trim().trim_matches('"').trim().to_string();
                if !n.is_empty() {
                    return n;
                }
            }
        }
    }

    let nickname = cli::config::default_server_username();
    let content = format!("# Butler workspace identity\nusername = \"{nickname}\"\n");
    let _ = std::fs::write(&config_path, content);
    nickname
}

// Public thin select (for compat in engine/handlers)
pub fn select_blocks(
    graph: &code_graph::CodeGraph,
    prompt: &str,
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
) -> Vec<code_graph::BlockInfo> {
    code_graph::snooper::select_blocks(graph, prompt, use_neural_scores, blend)
}

pub(super) fn selection_blend_from_settings(settings: &cli::config::ButlerSettings) -> NeuralSelectionBlend {
    NeuralSelectionBlend {
        text_weight: settings.agent.neural_text_weight,
        neural_weight: settings.agent.neural_score_weight,
    }
}

// --- Private helpers (cohesive, minimal clones) ---


/// Re-export: goal/mode → orchestrate path (see [`crate::server::mode_intent`]).
pub use crate::server::mode_intent::wants_orchestrate_path;


/// Core context retrieval logic (stage wire only — P3.1).
/// Delegates to ingress → project gate → load lobby → front door → serve prep →
/// surgical phase → compose path. Behavior identical to pre-stage peels.
pub fn run_context_logic(
    state: AppState,
    mut req: ContextRequest,
) -> Result<(StatusCode, Json<ContextResponse>), String> {
    let overall_start = Instant::now();

    // ── S0 ingress (caps, sanitize, help/meta/location, prompt) ──
    let IngressReady {
        force_surgical,
        effective_prompt,
        nl_guidance,
    } = match run_ingress(&mut req, overall_start) {
        IngressOutcome::Early(res) => return res,
        IngressOutcome::Ready(r) => r,
    };

    // ── S1 project gate (missing project, resolve, discovery) ──
    let ProjectGateReady { root, ipc_rules } =
        match try_project_gate(&state, &mut req, &effective_prompt, overall_start) {
            ProjectGateOutcome::Early(res) => return res,
            ProjectGateOutcome::Ready(r) => r,
        };

    // ── S2 load lobby (in-progress, async admit, pressure retry, layout defaults) ──
    let LoadLobbyReady {
        graph_rw,
        is_cached,
        graph_time_ms,
        node_count,
    } = match try_load_lobby(&state, &mut req, &root, overall_start) {
        LoadLobbyOutcome::Early(res) => return res,
        LoadLobbyOutcome::Ready(r) => r,
    };

    // ── S3 front door (before WarehousePolice / watcher / inventory rewalk) ──
    let FrontDoorContinue {
        query_key,
        edges_complete,
        edge_percent,
    } = match try_front_door(
        &state,
        &req,
        &root,
        &graph_rw,
        &effective_prompt,
        graph_time_ms,
        overall_start,
    ) {
        FrontDoorOutcome::Hit(res) => return res,
        FrontDoorOutcome::Continue(c) => c,
    };

    // ── S4 serve prep (watcher, bg edge, Phase-1/fast-fail, mode, neural) ──
    let prep = match try_serve_prep(
        &state,
        &req,
        &root,
        &graph_rw,
        force_surgical,
        &effective_prompt,
        graph_time_ms,
        is_cached,
        overall_start,
    ) {
        ServePrepOutcome::Early(res) => return res,
        ServePrepOutcome::Ready(r) => r,
    };

    // ── S5 surgical JIT + Phase-4 elaboration ──
    match run_surgical_phase(
        &state,
        &req,
        &root,
        &graph_rw,
        &ipc_rules,
        force_surgical,
        &prep,
        graph_time_ms,
        is_cached,
        overall_start,
    ) {
        SurgicalPhaseOutcome::Early(res) => return res,
        SurgicalPhaseOutcome::Continue => {}
    }

    // ── S6 compose path (serve read → lang void → Trace/Arch/compose → response) ──
    run_compose_path(
        &state,
        &req,
        &root,
        &graph_rw,
        &ipc_rules,
        force_surgical,
        &effective_prompt,
        &nl_guidance,
        &prep,
        graph_time_ms,
        is_cached,
        node_count,
        query_key,
        edges_complete,
        edge_percent,
        overall_start,
    )
}
