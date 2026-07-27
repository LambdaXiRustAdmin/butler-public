//! Project root resolution + monorepo auto-scope (P3 peel).
//! P3.1 S1: missing-project + discovery early exits ([`try_project_gate`]).
//! Zero intentional behavior change.

use std::path::{Path, PathBuf};
use std::time::Instant;

use axum::{http::StatusCode, Json};
use code_graph::snooper::normalize_path;

use crate::server::discovery::*;
use crate::server::dto::*;
use crate::server::paths::*;
use crate::server::render::render_shallow_marker_discovery;
use crate::server::scope::heal_scope_paths;
use crate::server::state::*;
use crate::vprintln;

pub(super) fn resolve_project_root(req: &mut ContextRequest) -> String {
    // Capture the user-provided path (after Docker translation) *before* anchoring.
    // This is the "originally requested subdirectory" for auto-scoping.
    let user_provided = if let Some(p) = &req.project {
        normalize_path(&translate_client_path(p.as_str()))
    } else {
        normalize_path(&translate_client_path(&req.root))
    };
    let user_path = Path::new(&user_provided).to_path_buf();

    // Existing resolution (name registry, BUTLER_PROJECTS_ROOT, canonicalize, etc.)
    let mut root = if let Some(p) = &req.project {
        let translated = normalize_path(&translate_client_path(p.as_str()));
        let path = Path::new(&translated);
        if path.is_absolute() {
            std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| resolve_project(translated.as_str()))
        } else {
            resolve_project(translated.as_str())
        }
    } else {
        let r = &req.root;
        let translated = normalize_path(&translate_client_path(r.as_str()));
        let path = Path::new(&translated);
        if path.is_absolute() {
            std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(translated)
        } else {
            translated
        }
    };

    if root == "." || root == "./" {
        if let Ok(cwd) = std::env::current_dir() {
            root = cwd.to_string_lossy().into_owned();
        }
    }

    let mut canonical = Path::new(&root).to_path_buf();

    // === Nearest package anchoring ===
    // Walk *up* from the user path and stop at the **nearest** project marker
    // (Cargo.toml, pyproject, …) or a .git dir if no marker is found first.
    //
    // Previously we preferred the *outermost* marker / parent .git. That consolidated
    // cache under monorepo roots but climbed past nested crates the user named
    // explicitly (e.g. pyo3/examples/word-count → pyo3), wiping isolation and
    // re-introducing the examples/ auto-ignore own-goal.
    let target_dir = if user_path.is_file() {
        user_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(user_path.clone())
    } else {
        user_path.clone()
    };
    let mut current = target_dir.clone();
    let mut nearest_root: Option<PathBuf> = None;
    loop {
        if has_any_marker(&current) || current.join(".git").exists() {
            nearest_root = Some(current.clone());
            break;
        }
        match current.parent() {
            Some(parent)
                if !parent.as_os_str().is_empty()
                    && parent != current
                    && parent != Path::new("/") =>
            {
                current = parent.to_path_buf();
            }
            _ => break,
        }
    }
    if let Some(n) = nearest_root {
        canonical = n;
        vprintln!(
            "📂 Project root (nearest marker): {} (user path was {})",
            canonical.display(),
            user_path.display()
        );
    }

    let true_root = canonical.to_string_lossy().into_owned();

    // === Auto-Scoping ===
    // If user pointed at a subdir *inside* the nearest root (no marker of its own),
    // inject that relative path into scope_paths for focused collection.
    if canonical != target_dir {
        if let Ok(relative) = target_dir.strip_prefix(&canonical) {
            let rel_str = normalize_path(&relative.to_string_lossy());
            if !rel_str.is_empty() {
                let scope_entry = if rel_str.ends_with('/') {
                    rel_str
                } else {
                    format!("{}/", rel_str)
                };
                match &mut req.scope_paths {
                    Some(v) => {
                        if !v.iter().any(|s| {
                            s == &scope_entry
                                || scope_entry.starts_with(s)
                                || s.starts_with(&scope_entry)
                        }) {
                            v.push(scope_entry);
                        }
                    }
                    None => {
                        req.scope_paths = Some(vec![scope_entry]);
                    }
                }
            }
        }
    }

    true_root
}

/// First-use defaults: noise ignore + monorepo scope when the agent sends nothing.
/// Call #1 on emscripten-scale trees must not require expert Butler knowledge.
pub(super) fn apply_first_use_layout_defaults(
    req: &mut ContextRequest,
    graph: &code_graph::CodeGraph,
    project_root: &Path,
) {
    let ignore_blank = req.ignore_paths.as_ref().map_or(true, |v| {
        v.is_empty()
            || v.iter().all(|s| {
                let t = s.trim();
                t.is_empty() || t == "." || t == "./"
            })
    });
    if ignore_blank {
        // L2.1: do **not** auto-ignore `examples/` — dual-stack packages under
        // monorepo examples/ (pyo3/examples/word-count) are interconnect gold and
        // already inventory-filtered (non-dual-stack tutorials still skip at scan).
        // Path ranking still demotes /examples/ for homonym preference.
        // A′.5: pybind binding roots keep `tests/` visible (m.def fixtures live there).
        // Same class as L2.1 not auto-ignoring examples/ for dual-stack packages.
        let pybind_root =
            code_graph::snooper::scanner::looks_pybind_binding_project(project_root);
        let mut ignores: Vec<String> = vec![
            "tools/".into(),
            "test/".into(),
            "benches/".into(),
            "bench/".into(),
            "docs/".into(),
            // Tutorial / doc-source trees (typer docs_src, sphinx-style docs_src, …).
            // Ranking already treats docs_src as noise; Trace/scope must too.
            "docs_src/".into(),
            "doc/".into(),
            "tutorials/".into(),
            "tutorial/".into(),
            "guides/".into(),
            "fixtures/".into(),
            "bindgen-tests/".into(),
            "samples/".into(),
            "demos/".into(),
            "testutil/".into(),
            "testing/".into(),
            "site/".into(),
        ];
        if !pybind_root {
            ignores.insert(1, "tests/".into());
        } else {
            vprintln!(
                "📦 First-use auto-ignore: keeping tests/ (pybind binding root — m.def fixtures)"
            );
        }
        // Bundled-vendor segments (vendor, _vendor, _click, third_party, …) —
        // built-in list from code_graph. Users extend via
        // analysis.extra_bundled_vendor_segments (scan skip + noise); first-use
        // uses the built-in set here (segment-exact ignore_paths).
        for pat in code_graph::bundled_vendor_skip_patterns() {
            if !ignores.iter().any(|i| i == &pat) {
                ignores.push(pat);
            }
        }
        // Own-goal: project *is* under examples/ (or tools/, docs/) → never auto-ignore that segment.
        // Same class as scope_paths:["tools/…"] hole. Gold FFI keepers live in examples/word-count.
        let before_root = ignores.len();
        ignores.retain(|ign| !project_root_overlaps_auto_ignore(project_root, ign));
        if ignores.len() < before_root {
            vprintln!(
                "📦 First-use auto-ignore: dropped {} pattern(s) under project root path",
                before_root - ignores.len()
            );
        }
        // Explicit scope_paths: never auto-ignore a segment the user scoped into.
        if let Some(scopes) = req.scope_paths.as_ref() {
            let before = ignores.len();
            ignores.retain(|ign| !scope_overlaps_auto_ignore(scopes, ign));
            if ignores.len() < before {
                vprintln!(
                    "📦 First-use auto-ignore: dropped {} noise pattern(s) that overlap explicit scope_paths",
                    before - ignores.len()
                );
            }
        }
        req.ignore_paths = Some(ignores);
        vprintln!(
            "📦 First-use auto-ignore noise dirs (blank ignore_paths) for {} blocks",
            graph.nodes.len()
        );
    }

    apply_monorepo_auto_scope(req, graph, project_root);
}

/// True when auto-ignore pattern would exclude something the user asked to scope.
pub(super) fn scope_overlaps_auto_ignore(scopes: &[String], ignore_pat: &str) -> bool {
    let ign = normalize_path(ignore_pat)
        .trim_end_matches('/')
        .to_string();
    if ign.is_empty() {
        return false;
    }
    for sc in scopes {
        let s = normalize_path(sc);
        // scope is the ignore dir, under it, or a file inside it
        if s == ign
            || s == format!("{ign}/")
            || s.starts_with(&format!("{ign}/"))
            || s.contains(&format!("/{ign}/"))
            || s.ends_with(&format!("/{ign}"))
        {
            return true;
        }
        // path segment match: tools/link.py has segment "tools"
        if s.split('/').any(|seg| seg == ign) {
            return true;
        }
    }
    false
}

/// Project root lives under `examples/`, `tools/`, etc. → that segment is not noise.
pub(super) fn project_root_overlaps_auto_ignore(project_root: &Path, ignore_pat: &str) -> bool {
    let ign = normalize_path(ignore_pat)
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if ign.is_empty() {
        return false;
    }
    project_root.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| s.eq_ignore_ascii_case(&ign))
            .unwrap_or(false)
    })
}

/// Large monorepos: default scope when the agent sends nothing.
/// Language-agnostic spine (see [`crate::server::monorepo_scope`]).
pub(super) fn apply_monorepo_auto_scope(
    req: &mut ContextRequest,
    graph: &code_graph::CodeGraph,
    project_root: &Path,
) {
    if !crate::server::filters::is_blank_scope(&req.scope_paths) {
        return;
    }

    let want_arch_spine = {
        use crate::server::mode_intent::{intent_from_request, ModeIntent};
        let is_orch = req.mcp_tool_name.as_deref() == Some("butler_orchestrate");
        let intent = intent_from_request(req);
        if is_orch {
            // Blank goal on orchestrate tool → default arch spine (historical).
            matches!(intent, ModeIntent::Unknown) || intent.is_architectural_summary()
        } else {
            intent.is_architectural_summary()
        }
    };

    let Some(plan) =
        crate::server::monorepo_scope::plan_monorepo_scopes(graph, project_root, want_arch_spine)
    else {
        return;
    };

    // Extra noise floors always useful on large trees (fixtures drown workspaces).
    // L2.1: do **not** re-add `examples/` here — dual-stack packages under monorepo
    // examples/ must stay visible for unscoped Trace (inventory already filters
    // non-dual-stack tutorials). First-use defaults also omit examples/.
    if let Some(ignores) = req.ignore_paths.as_mut() {
        for extra in [
            "fixtures/",
            "bindgen-tests/",
            "docs_src/",
            "doc/",
            "tutorials/",
            "tutorial/",
            "guides/",
            "samples/",
            "demos/",
            "regress/",
            "fuzz/",
        ] {
            if !ignores.iter().any(|i| i == extra) {
                // Only add if not overlapping a chosen scope
                if plan.fail_open || !scope_overlaps_auto_ignore(&plan.scopes, extra) {
                    ignores.push(extra.into());
                }
            }
        }
        if !plan.fail_open {
            ignores.retain(|ign| !scope_overlaps_auto_ignore(&plan.scopes, ign));
        }
    }

    if plan.fail_open {
        // False open: no scope_paths — whole graph minus ignores (flat C / low confidence).
        // Arch leviathans: agent_suggestions only (never auto-cage fat top-level dirs).
        if !plan.agent_suggestions.is_empty() {
            vprintln!(
                "📦 Monorepo auto-scope ({} blocks, fail_open, {}) → (none) ignore={:?} \
                 suggested_scopes={:?}",
                graph.nodes.len(),
                plan.reason,
                req.ignore_paths,
                plan.agent_suggestions
            );
        } else {
            vprintln!(
                "📦 Monorepo auto-scope ({} blocks, fail_open, {}) → (none) ignore={:?}",
                graph.nodes.len(),
                plan.reason,
                req.ignore_paths
            );
        }
        return;
    }

    let selected = plan.scopes;
    vprintln!(
        "📦 Monorepo auto-scope ({} blocks, spine_first={}, {}) → {:?} ignore={:?}",
        graph.nodes.len(),
        plan.spine_first,
        plan.reason,
        selected,
        req.ignore_paths
    );
    req.scope_paths = Some(selected);
}

/// True when the request should use the orchestrate seed path (Trace/Find/Arch).
///
/// Covers MCP `butler_orchestrate` **and** bare `POST /context` with the same goals
/// so short PascalCase hubs (Group, Typer) get `blocks_for_name` Trace, not fail-closed
/// select_blocks-only.
/// Apply `butler_ask` façade routing on a deserialized HTTP request.
pub(super) fn normalize_butler_ask_request(req: &mut ContextRequest) {
    if req.mcp_tool_name.as_deref() != Some("butler_ask") {
        return;
    }
    // query → symbol or prompt
    if let Some(q) = req.query.take() {
        let q = q.trim().to_string();
        if !q.is_empty() {
            let empty_sym = req
                .target_symbol
                .as_ref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            if empty_sym && cli::butler_ask::looks_like_symbol_token(&q) {
                req.target_symbol = Some(q.clone());
                if req.prompt.trim().is_empty() {
                    req.prompt = q;
                }
            } else if req.prompt.trim().is_empty() {
                req.prompt = q;
            }
        }
    }
    if let Some(sym) = req.target_symbol.clone() {
        if req.prompt.trim().is_empty() {
            req.prompt = sym;
        }
    }
    let mode = req.mode.as_deref().unwrap_or("auto");
    let mode_l = mode.to_ascii_lowercase();
    // Façade short forms only (see mode_intent::is_butler_ask_facade_mode) — don't clobber
    // already-normalized orchestrate goals. Full words like "architecture" bypass remap.
    let façade = crate::server::mode_intent::is_butler_ask_facade_mode(&mode_l)
        || req.goal.is_none();
    if façade {
        let has_symbol = req
            .target_symbol
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty());
        let has_scope = req
            .scope_paths
            .as_ref()
            .is_some_and(|a| !a.is_empty());
        let has_file_line = req.target_file.is_some() && req.target_line.is_some();
        let q = if !req.prompt.trim().is_empty() {
            Some(req.prompt.as_str())
        } else {
            None
        };
        let goal = cli::butler_ask::route_ask_goal(&mode_l, has_symbol, has_scope, has_file_line, q);
        // Dual-write: both fields must agree after façade routing. Downstream
        // intent_from_request prefers goal then mode (.or); keep them identical.
        req.goal = Some(goal.to_string());
        req.mode = Some(goal.to_string());
    }
    if req.detail.is_none() {
        req.detail = Some("compact".into());
    }
    req.mcp_tool_name = Some("butler_orchestrate".into());
}

// --- S1 project gate (missing project / resolve / discovery) ---

/// Resolved root + per-project IPC rules after a successful project gate.
pub(super) struct ProjectGateReady {
    pub root: String,
    pub ipc_rules: Vec<code_graph::snooper::ipc_engine::IpcRule>,
}

pub(super) enum ProjectGateOutcome {
    Early(Result<(StatusCode, Json<ContextResponse>), String>),
    Ready(ProjectGateReady),
}

/// Missing `project`, resolve root, heal scopes, discovery-mode early exits.
pub(super) fn try_project_gate(
    state: &AppState,
    req: &mut ContextRequest,
    effective_prompt: &str,
    overall_start: Instant,
) -> ProjectGateOutcome {
    if req.project.is_none() {
        let error_msg = r#"[ERROR] The 'project' parameter is required.

You can either:
- Call `butler_select_project` to discover projects under BUTLER_PROJECTS_ROOT, **or**
- Pass an **absolute path** directly (works for any external repo on disk):

Example for an external repository:
{
  "project": "/projects/test_repos/fd",
  "prompt": "main or cli",
  "mode": "balanced"
}

Or use "root" for the path while putting any name in "project".
"#;
        return ProjectGateOutcome::Early(Ok((
            StatusCode::OK,
            Json(crate::server::filters::degenerate_context_response(
                error_msg.to_string(),
                Some("missing required project parameter".to_string()),
                Some("missing_project".to_string()),
                0,
                false,
                overall_start.elapsed().as_millis() as u64,
            )),
        )));
    }

    // Use helper (returns owned for now to keep downstream simple; internal Cow used)
    // NOTE: pass &mut so anchoring can inject auto-scope_paths if we had to walk up.
    let root = resolve_project_root(req);
    heal_scope_paths(&mut req.scope_paths, Path::new(&root));

    let ipc_rules = {
        let mut settings = state.settings.clone();
        settings.merge_project_config(Path::new(&root));
        settings.ipc_rules_for_engine()
    };

    let client_project = req.project.as_deref().unwrap_or(&req.root);
    let was_dot_project =
        client_project == "." || client_project == "./" || client_project.is_empty();

    let target_symbol = req.target_symbol.as_deref().unwrap_or("");
    vprintln!(
        "📥 REQUEST → original=\"{}\" effective=\"{}\" target_symbol=\"{}\" root=\"{}\"",
        req.prompt, effective_prompt, target_symbol, root
    );

    let is_non_orchestrate = req.mcp_tool_name.as_deref() != Some("butler_orchestrate");
    if was_dot_project || (is_non_orchestrate && should_use_discovery_for_root(&root)) {
        let listing = render_shallow_marker_discovery(Path::new(&root));
        let content = format!(
            "No specific project provided. Here are the available projects. Please call the tool again with one of these as your `project` or use `butler_orchestrate` with `scope_paths`.\n\n{}",
            listing
        );
        return ProjectGateOutcome::Early(Ok((
            StatusCode::OK,
            Json(crate::server::filters::degenerate_context_response(
                content,
                Some("discovery_mode".to_string()),
                Some("discovery".to_string()),
                0,
                false,
                overall_start.elapsed().as_millis() as u64,
            )),
        )));
    }

    if should_use_discovery_for_root(&root) {
        let listing = render_shallow_marker_discovery(Path::new(&root));
        // Loud path already in listing when missing/empty; keep short header otherwise.
        let content = if listing.contains("Does not appear to be a valid project")
            || listing.contains("Does not appear to be a useful project")
        {
            listing
        } else {
            format!(
                "### Butler Discovery Mode (shallow 2-level FS listing — no Tree-sitter scan performed)\n\n{}\n\nProvide one of the highlighted marker folders as your `project` (preferred) or in `scope_paths` for a focused ArchitecturalSummary/context.\n",
                listing
            )
        };
        return ProjectGateOutcome::Early(Ok((
            StatusCode::OK,
            Json(crate::server::filters::degenerate_context_response(
                content,
                Some("discovery_mode".to_string()),
                Some("discovery".to_string()),
                0,
                false,
                overall_start.elapsed().as_millis() as u64,
            )),
        )));
    }

    ProjectGateOutcome::Ready(ProjectGateReady { root, ipc_rules })
}

