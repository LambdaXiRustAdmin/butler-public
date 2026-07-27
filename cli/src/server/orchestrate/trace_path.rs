//! Trace/Find path (Thesis O3 peel).
//! Memo hit · seed · T.2 disambiguate · BFS · pack · Soft I4 · telemetry · memo store.
//! Zero intentional behavior change.

use crate::server::build_status;
use crate::server::dto::*;
use crate::server::state::AppState;
use crate::vprintln;
use code_graph::{BlockInfo, CodeGraph, NeuralSelectionBlend};
use std::path::Path;
use std::time::Instant;

use super::disambiguate::{
    collision_alt_file_count, needs_homonym_disambiguation, pin_locations_for_disambiguate,
    sanitize_scope_prefix, serious_alt_file_count, suggested_scopes_from_locations,
    suggested_scopes_from_paths,
};
use super::helpers::{
    bridge_infos, cluster_infos_from_scoped, collect_seed_bridge_neighbors, hop_split,
    lang_cluster_of, scope_frame_line_with_peers,
};
use super::inject_response_telemetry;
use super::neighborhood;
use super::outputs::make_state_info;
use super::peer_callers;
use super::receipt::{attach_why_edges, next_action_disambiguate, set_next_action};
use super::render::ContentDetail;
use super::seed;
use super::spine;
use super::trace_pack_helpers::{
    build_symbol_locations, cap_trace_payload_focus, enclosing_callable,
    loc_fallback_unique_fn_def, next_action_mega_hub, HUB_FANIN_NEXT,
};

/// Run TraceBlastRadius / FindImplementation arm.
/// Returns `(structured, mermaid)` — miss / empty handled by caller finalizer.
pub(crate) fn run_trace_find(
    req: &ContextRequest,
    state: &AppState,
    root: &str,
    root_path: &Path,
    graph: &CodeGraph,
    scoped: &[&BlockInfo],
    noise_cfg: &crate::server::filters::NoiseFilterConfig,
    use_neural_scores: bool,
    blend: NeuralSelectionBlend,
    goal_str: &str,
    _edge_build_label: &str,
    jit_note: &str,
    bg_pct: usize,
    bg_status: &str,
    edges_complete: bool,
    blocks_scanned: usize,
    total_time_ms: u64,
    pp: &code_graph::ProjectPaths,
    ipc_rules: &[code_graph::snooper::ipc_engine::IpcRule],
) -> (Option<StructuredReport>, Option<String>) {
    let mut trace_mermaid: Option<String> = None;
    let mut structured: Option<StructuredReport> = None;

    let symbol = req
        .target_symbol
        .clone()
        .unwrap_or_else(|| req.prompt.clone());
    if symbol.trim().is_empty() {
        // placeholder, but we still build json below
    } else {
        // ── Trace memo: remember the tour when the building is unchanged ──
        let memo_epoch = crate::server::trace_memo::graph_epoch(graph);
        let memo_key = crate::server::trace_memo::make_trace_key_from_req(
            root,
            goal_str,
            symbol.trim(),
            memo_epoch,
            req,
            state.settings.analysis.trace_max_fan_out,
            state.settings.analysis.trace_max_visited_nodes,
            2,
        );
        let mut memo_hit = false;
        if let Some(payload) =
            crate::server::trace_memo::lookup(Path::new(root), memo_key, memo_epoch)
        {
            let conf = build_status::confidence_rung(
                state,
                root,
                Some(graph),
                true,
                !payload.callers.is_empty() || !payload.callees.is_empty(),
            );
            let st = make_state_info(
                format!("{}% | {}", bg_pct, bg_status),
                jit_note.to_string(),
                conf,
                bg_pct,
            );
            let (mut rep, mer) = crate::server::trace_memo::report_from_memo(
                &payload,
                st,
                &jit_note,
                total_time_ms,
                blocks_scanned,
            );
            let focus_names = crate::server::trace_pack::focus_names_from_parts(
                req.focus_symbol.as_deref(),
                req.focus_symbols.as_ref().map(|v| v.as_slice()),
            );
            crate::server::trace_pack::stamp_focus_telemetry(
                &mut rep.telemetry,
                &rep.callers,
                &focus_names,
            );
            crate::server::trace_pack::stamp_sample_window_telemetry(
                &mut rep.telemetry,
                req.sample_offset,
                req.sample_mode.as_deref(),
                req.exclude_symbols.as_ref().map(|v| v.as_slice()),
            );
            // Honesty: scrub peer∩hard even on memo tours (pre-v18 packs).
            let _ = peer_callers::dedupe_peers_against_hard_callers(
                &mut rep.peer_callers,
                &rep.callers,
            );
            // Live reverse spine on memo hit (pre-v9 memos / empty path).
            if goal_str == "TraceBlastRadius" && rep.caller_path.is_empty() {
                rep.caller_path = spine::reverse_call_spine_for_seed(
                    graph,
                    Path::new(root),
                    &payload.target.seed_id,
                    &payload.target.name,
                    &payload.target.file,
                    &rep.callers,
                );
            }
            structured = Some(rep);
            trace_mermaid = mer;
            memo_hit = true;
            vprintln!(
                "⚡ Trace memo HIT key={:#x} epoch={:#x} symbol={:?} (skip seed+BFS)",
                memo_key,
                memo_epoch,
                symbol.trim()
            );
        }

        if memo_hit {
            // Tour restored from disk — skip walk.
        } else {
        // Seed selection cascade (exact → fuzzy → module shell) — `seed` module.
        let mut prof: Vec<(&str, f64)> = Vec::with_capacity(16);
        let t_trace = Instant::now();
        let t_seed = Instant::now();
        let seed_res = seed::resolve_trace_seed(
            graph,
            scoped,
            symbol.as_str(),
            root_path,
            &req.scope_paths,
            &req.ignore_paths,
            use_neural_scores,
            blend,
        );
        prof.push(("seed_index_rank", t_seed.elapsed().as_secs_f64() * 1000.0));
        // module_shell timing folded into seed cascade (same work as before).
        prof.push(("module_shell", 0.0));
        let target_opt = seed_res.target;
        let module_resolved_from = seed_res.module_resolved_from;
        let module_interior_candidates = seed_res.module_interior_candidates;

        if let Some(target) = target_opt {
            // Target definition (capped) for JSON target.definition. No MD snippet.
            let t_body = Instant::now();
            let body = code_graph::snooper::token_manager::truncate_to_token_cap(
                &target.source,
                1500,
            );
            prof.push(("body_trunc", t_body.elapsed().as_secs_f64() * 1000.0));

            // Locations early (before BFS) — T.2 alts-first gate for mega-homonyms.
            let t_loc_early = Instant::now();
            let locations = build_symbol_locations(
                graph,
                target,
                scoped,
                symbol.trim(),
                root_path,
                16,
            );
            prof.push(("locations", t_loc_early.elapsed().as_secs_f64() * 1000.0));

            let scope_slice = req.scope_paths.as_deref();
            if needs_homonym_disambiguation(symbol.trim(), &locations, scope_slice) {
                // Prefer serious defs for pins; fall back to collision hits (lets/mods)
                // when multi-loc is only non-seed tiers (bevy `app`).
                let pin_locs = pin_locations_for_disambiguate(&locations);
                let n_files = serious_alt_file_count(&locations)
                    .max(collision_alt_file_count(&locations));
                let (t_lang, t_cluster) = lang_cluster_of(target);
                let conf = build_status::confidence_rung(
                    state,
                    root,
                    Some(graph),
                    true,
                    false, // no neighborhood yet — disambiguate first
                );
                // Force medium trust band even if edges_full (do not lean on ★ yet)
                let conf_label = "index_exact"; // medium
                let mut def_body = body;
                if def_body.trim().is_empty() {
                    if let Some(s) = crate::server::filters::cite_snippet_from_disk(
                        root_path,
                        &pp.to_display(&target.file),
                        target.start_line,
                    ) {
                        def_body = s;
                    }
                }
                let target_info = TargetInfo {
                    name: target.name.clone(),
                    file: pp.to_display(&target.file),
                    line: target.start_line,
                    definition: if def_body.trim().is_empty() {
                        None
                    } else {
                        Some(def_body)
                    },
                    lang: Some(t_lang),
                    cluster: Some(t_cluster.clone()),
                };
                let suggested =
                    suggested_scopes_from_locations(root_path, &pin_locs, 8);
                let mut telemetry = serde_json::json!({
                    "disambiguate": true,
                    "serious_alt_files": n_files,
                    "collision_alt_files": collision_alt_file_count(&locations),
                    "locations_count": locations.len(),
                    "homonym_risk": true,
                    "edges_complete": edges_complete,
                    "percent": bg_pct,
                    "confidence": conf_label,
                    "action": "pin_scope_paths_or_pick_location",
                });
                inject_response_telemetry(
                    &mut telemetry,
                    blocks_scanned,
                    total_time_ms,
                    1 + locations.len().min(16),
                );
                let err = format!(
                    "disambiguate: '{}' has {} serious production locations — pin scope_paths (file or dir) or re-query a path-qualified symbol before Trace neighborhood",
                    symbol.trim(),
                    n_files
                );
                let mut disambig_st = StructuredReport {
                    state: StateInfo {
                        edge_build: format!("{}% | {}", bg_pct, bg_status),
                        jit: jit_note.to_string(),
                        confidence: Some(conf_label.into()),
                        percent: Some(bg_pct),
                    },
                    error: Some(err),
                    target: Some(target_info),
                    callers: vec![],
                    callees: vec![],
                    caller_path: vec![],
                    peer_callers: vec![],
                    bridge_callers: vec![],
                    bridge_callees: vec![],
                    blast_domain: Some("disambiguate".into()),
                    seed_kind: Some(target.kind.clone()),
                    receipt: None,
                    next_action: None,
                    telemetry,
                    suggested_scopes: suggested,
                    skeleton: None,
                    hubs: None,
                    module_resolved_from: None,
                    module_interior_candidates: None,
                    locations: Some(locations),
                    clusters: None,
                    bridges: None,
                    active_cluster: Some(t_cluster),
                };
                set_next_action(&mut disambig_st, next_action_disambiguate());
                structured = Some(disambig_st);
                let _ = conf; // ladder computed for future; we force medium label
                vprintln!(
                    "⚡ T.2 disambiguate symbol={:?} serious_files={} (skip BFS)",
                    symbol.trim(),
                    n_files
                );
            } else {

            let t_bfs = Instant::now();
            let scope_for_neigh: Vec<String> = req
                .scope_paths
                .as_ref()
                .map(|v| {
                    v.iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            // Multi-hop hard cap 2 (see trace_pack::resolve_trace_depth).
            let (trace_depth, expand_hops_meta) =
                crate::server::trace_pack::resolve_trace_depth(req.depth, req.expand_hops);
            let neigh = neighborhood::expand_trace_neighborhood(
                graph,
                target,
                root_path,
                noise_cfg,
                req.compress_tests,
                trace_depth,
                state.settings.analysis.max_call_graph_depth,
                state.settings.analysis.trace_max_fan_out,
                state.settings.analysis.trace_max_visited_nodes,
                &scope_for_neigh,
            );
            let callers_by_depth = neigh.callers_by_depth;
            let callees_by_depth = neigh.callees_by_depth;
            let peer_caller_pairs = neigh.peer_callers;
            let _test_callers_omitted = neigh.test_callers_omitted;
            let _test_callees_omitted = neigh.test_callees_omitted;
            let blast_depth = neigh.blast_depth;
            let trace_stats = neigh.stats;
            let max_fan_out = neigh.max_fan_out;
            let max_visited_nodes = neigh.max_visited_nodes;
            let is_noisy =
                |name: &str| crate::server::filters::is_trace_noise_name(name);
            prof.push(("bfs_l1_l2", t_bfs.elapsed().as_secs_f64() * 1000.0));
            // trim folded into expand_trace_neighborhood
            prof.push(("trim", 0.0));

            // Task 3: Grouped Traces (partition by structural_multiplier for curated LLM output)
            let t_part = Instant::now();
            let (core_callers, core_callees, utility_omitted) =
                crate::server::filters::partition_trace_cores(
                    &callers_by_depth,
                    &callees_by_depth,
                    graph,
                    root_path,
                );
            prof.push(("partition", t_part.elapsed().as_secs_f64() * 1000.0));

            // locations already built pre-BFS (T.2). Cap already 16.

            let mut pack_callers = core_callers;
            let pack_callees = core_callees;

            // Location fallbacks (CALL-shaped) before packing empty caller side.
            //
            // **Honesty gate (class L):** call-site locations are same-name *calls*,
            // not reverse edges into ★. On multi-def / multi-file homonyms this invents
            // hard CALL parents of twin A from callers of twin B (fmt parse/get, wasmtime
            // builder). Silence > invent: only allow when the seed name has a **unique**
            // function-like def (single file). Peer twin recovery stays in `peer_callers`.
            //
            // Also skip **type** seeds (Console/Group call-site flood — A′.10).
            let t_fb = Instant::now();
            let type_seed =
                crate::server::filters::is_type_trace_target(&target.kind);
            let mut callers_loc_fallback = false;
            let mut callers_loc_fallback_names: Vec<String> = Vec::new();
            let loc_fallback_ok = pack_callers.is_empty()
                && !type_seed
                && loc_fallback_unique_fn_def(graph, target);
            if loc_fallback_ok {
                let mut seen = std::collections::HashSet::new();
                for loc in &locations {
                    let k = loc.kind.to_ascii_lowercase();
                    if !(k.contains("call") || k == "call") {
                        continue;
                    }
                    // Demote benchmark/test trees when recovering callers from alts.
                    let loc_path = loc.file.replace('\\', "/").to_ascii_lowercase();
                    if loc_path.contains("/benchmarks/")
                        || loc_path.contains("/benchmark/")
                        || loc_path.contains("/benches/")
                        || loc_path.contains("/tests/")
                        || loc_path.contains("/test/")
                    {
                        continue;
                    }
                    if let Some(enc) =
                        enclosing_callable(graph, &loc.file, loc.line, root_path)
                    {
                        if enc.id == target.id || !seen.insert(enc.id.clone()) {
                            continue;
                        }
                        if crate::server::filters::is_testish_seed_block(enc) {
                            continue;
                        }
                        if callers_loc_fallback_names.len() < 12 {
                            callers_loc_fallback_names.push(enc.name.clone());
                        }
                        pack_callers.push(
                            crate::server::filters::caller_callee_from_block(enc, pp),
                        );
                        callers_loc_fallback = true;
                    }
                }
            }
            prof.push(("loc_fallback", t_fb.elapsed().as_secs_f64() * 1000.0));

            // P.4: typed bridges separate from CALL pack.
            let t_br = Instant::now();
            let (mut bridge_callers, mut bridge_callees) =
                collect_seed_bridge_neighbors(graph, target, pp, &is_noisy);
            // IPC rule fallback → bridge_callers (not CALL). Disk re-read for slim.
            if bridge_callers.is_empty() && !ipc_rules.is_empty() {
                for id in code_graph::snooper::ipc_engine::find_ipc_caller_ids_with_root(
                    graph,
                    ipc_rules,
                    &target.name,
                    scoped,
                    Some(root_path),
                ) {
                    let Some(b) = graph.get_block(id) else {
                        continue;
                    };
                    let mut cc =
                        crate::server::filters::caller_callee_from_block(b, pp);
                    cc.relation = Some("ipc".into());
                    bridge_callers.push(cc);
                }
            }
            prof.push(("bridge_neighbors", t_br.elapsed().as_secs_f64() * 1000.0));

            // Comprehensive hop split on **pre-pack** neighborhood (not the sample).
            let (callers_direct, callers_transitive) = hop_split(&pack_callers);
            let (callees_direct, callees_transitive) = hop_split(&pack_callees);

            // Dossier pack: char budget + min-1 per side + hard ceiling fuse.
            // Scope prefixes bias sample toward agent pin (de-god under scope).
            let t_pack = Instant::now();
            let max_blocks = state.settings.analysis.max_context_blocks;
            let scope_prefs: Vec<String> = req
                .scope_paths
                .as_ref()
                .map(|v| {
                    v.iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let detail_mode = ContentDetail::from_req(req.detail.as_deref());
            let focus_names = crate::server::trace_pack::focus_names_from_parts(
                req.focus_symbol.as_deref(),
                req.focus_symbols.as_ref().map(|v| v.as_slice()),
            );
            let exclude_names = crate::server::trace_pack::normalize_exclude_symbols(
                req.exclude_symbols.as_ref().map(|v| v.as_slice()),
            );
            // P1: dir facets from pre-pack warehouse reverse (pin mitigation).
            let caller_dir_facets =
                crate::server::trace_pack::caller_dir_facets(&pack_callers, 6);
            let (mut pack, focus_injected, window_meta) = cap_trace_payload_focus(
                pack_callers,
                pack_callees,
                graph,
                max_blocks,
                &scope_prefs,
                detail_mode.is_long(),
                &focus_names,
                req.sample_offset,
                req.sample_mode.as_deref(),
                &exclude_names,
            );
            let focus_missed =
                crate::server::trace_pack::focus_missed(&focus_names, &focus_injected);
            // Cite pack: top neighbors get quoteable source (disk if slim-stripped).
            crate::server::filters::fill_cites_from_disk(
                &mut pack.callers,
                root_path,
                3,
            );
            crate::server::filters::fill_cites_from_disk(
                &mut pack.callees,
                root_path,
                3,
            );
            // T.1c why-edge: honest proof only on top-3 (silence if none)
            attach_why_edges(
                &target.name,
                &mut pack.callers,
                &mut pack.callees,
                &mut bridge_callers,
                &mut bridge_callees,
            );
            prof.push(("pack", t_pack.elapsed().as_secs_f64() * 1000.0));
            let (t_lang, t_cluster) = lang_cluster_of(target);
            let blast_domain =
                crate::server::filters::blast_domain_for_seed_kind(&target.kind);
            let mut def_body = body;
            if def_body.trim().is_empty() {
                if let Some(s) = crate::server::filters::cite_snippet_from_disk(
                    root_path,
                    &pp.to_display(&target.file),
                    target.start_line,
                ) {
                    def_body = s;
                }
            }
            let target_info = TargetInfo {
                name: target.name.clone(),
                file: pp.to_display(&target.file),
                line: target.start_line,
                definition: if def_body.trim().is_empty() {
                    None
                } else {
                    Some(def_body)
                },
                lang: Some(t_lang),
                cluster: Some(t_cluster.clone()),
            };

            let (shown_cr_d, shown_cr_t) = hop_split(&pack.callers);
            let (shown_ce_d, shown_ce_t) = hop_split(&pack.callees);
            // Warehouse **direct** reverse into ★ only (not same-name peers).
            let warehouse_in = graph.callers(&target.id).len();
            let warehouse_peer_in = peer_caller_pairs.len();
            let warehouse_out = graph.children(&target.id).len();
            let listed_hop1_callers = pack
                .callers
                .iter()
                .filter(|c| c.hop <= 1)
                .count();
            // Scope “called by N” = hard CALL into ★ (+ loc-fallback rows in pack).
            let seed_in_degree = warehouse_in.max(listed_hop1_callers);
            let seed_out_degree = warehouse_out;
            // Labeled peer reverse (twin-id recovery) — not mixed into seed_in_degree.
            let mut peer_callers = peer_callers::peer_callers_to_rows(
                graph,
                &peer_caller_pairs,
                pp,
                peer_callers::PEER_CALLERS_SAMPLE_CAP,
            );
            // Honesty de-dupe: never list the same (name, file) as both hop-1 hard
            // CALL and name_peer (wasmtime builder pin trap / twin-id display clash).
            let peer_hard_deduped = peer_callers::dedupe_peers_against_hard_callers(
                &mut peer_callers,
                &pack.callers,
            );
            // I4 honesty: omitted = warehouse fan-in − shown, not only packer
            // truncate of an already-pruned list (that lied at omitted=0 with
            // seed_in_degree=15 / shown=12).
            let callers_omitted = seed_in_degree
                .saturating_sub(pack.callers.len())
                .max(pack.callers_omitted());
            let callees_omitted = seed_out_degree
                .saturating_sub(pack.callees.len())
                .max(pack.callees_omitted());
            let callers_total = seed_in_degree.max(pack.callers_total);
            let callees_total = seed_out_degree.max(pack.callees_total);
            let scope_frame = scope_frame_line_with_peers(
                seed_in_degree,
                seed_out_degree,
                warehouse_peer_in,
                trace_stats.fan_out_pruned,
                trace_stats.visited_capped,
                edges_complete,
                bridge_callers.len() + bridge_callees.len(),
            );
            let mut telemetry = serde_json::json!({
                "depth": blast_depth,
                "slicing": "trace_packer_char_budget",
                "blast_domain": blast_domain,
                "seed_kind": target.kind,
                "max_fan_out": max_fan_out,
                "max_visited_nodes": max_visited_nodes,
                "trace_nodes_visited": trace_stats.nodes_visited,
                "locations_count": locations.len(),
                // Warehouse fan-in/out as total; pack sample in shown/omitted.
                "callers_total": callers_total,
                "callees_total": callees_total,
                "callers_shown": pack.callers.len(),
                "callees_shown": pack.callees.len(),
                "callers_direct": callers_direct,
                "callers_transitive": callers_transitive,
                "callees_direct": callees_direct,
                "callees_transitive": callees_transitive,
                "callers_shown_direct": shown_cr_d,
                "callers_shown_transitive": shown_cr_t,
                "callees_shown_direct": shown_ce_d,
                "callees_shown_transitive": shown_ce_t,
                "seed_in_degree": seed_in_degree,
                "seed_in_degree_warehouse": warehouse_in,
                "seed_in_degree_name_peers": warehouse_peer_in,
                "peer_callers_shown": peer_callers.len(),
                "peer_hard_deduped": peer_hard_deduped,
                "seed_out_degree": seed_out_degree,
                "callers_loc_fallback": callers_loc_fallback,
                "callers_loc_fallback_names": callers_loc_fallback_names,
                "scope_frame": scope_frame,
                "bridge_callers": bridge_callers.len(),
                "bridge_callees": bridge_callees.len(),
                "callers_omitted": callers_omitted,
                "callees_omitted": callees_omitted,
                "pack_chars_used": pack.chars_used,
                "pack_char_budget": if detail_mode.is_long() {
                    crate::server::trace_pack::LONG_CHAR_BUDGET
                } else {
                    crate::server::trace_pack::SHORT_CHAR_BUDGET
                },
                "edge_census": "shown_vs_warehouse",
                "hub_scale": seed_in_degree >= HUB_FANIN_NEXT
                    || seed_out_degree >= HUB_FANIN_NEXT,
                "pack_scope_bias": !scope_prefs.is_empty(),
                "detail_length": detail_mode.as_length_label(),
                "pack_hard_ceiling": if detail_mode.is_long() {
                    crate::server::trace_pack::LONG_HARD_CEILING
                } else {
                    crate::server::trace_pack::SHORT_HARD_CEILING
                },
            });
            // Focus / expand_hops / sample window — insert after json! (macro recursion).
            if let Some(obj) = telemetry.as_object_mut() {
                if let Some(fs) = req.focus_symbol.as_ref() {
                    obj.insert("focus_symbol".into(), serde_json::json!(fs));
                }
                obj.insert("focus_symbols".into(), serde_json::json!(focus_names));
                obj.insert("focus_injected".into(), serde_json::json!(focus_injected));
                obj.insert("focus_missed".into(), serde_json::json!(focus_missed));
                obj.insert(
                    "sample_offset".into(),
                    serde_json::json!(window_meta.sample_offset),
                );
                obj.insert(
                    "sample_mode".into(),
                    serde_json::json!(window_meta.sample_mode.as_str()),
                );
                obj.insert(
                    "exclude_symbols".into(),
                    serde_json::json!(exclude_names),
                );
                obj.insert(
                    "exclude_count".into(),
                    serde_json::json!(window_meta.exclude_count),
                );
                obj.insert(
                    "callers_ranked".into(),
                    serde_json::json!(window_meta.callers_ranked),
                );
                obj.insert(
                    "callees_ranked".into(),
                    serde_json::json!(window_meta.callees_ranked),
                );
                obj.insert(
                    "sample_window_exhausted".into(),
                    serde_json::json!(window_meta.sample_window_exhausted),
                );
                obj.insert(
                    "caller_dir_facets".into(),
                    serde_json::json!(caller_dir_facets),
                );
                if let Some(m) = expand_hops_meta.as_object() {
                    for (k, v) in m {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
            if let Some(reason) = pack.truncation_reason {
                telemetry["truncation_reason"] = reason.into();
            }
            // Always emit prune flags (0 = complete neighborhood under caps).
            telemetry["fan_out_pruned"] = trace_stats.fan_out_pruned.into();
            if trace_stats.visited_capped {
                telemetry["visited_capped"] = true.into();
            }
            if utility_omitted > 0 {
                telemetry["utility_omitted"] = utility_omitted.into();
            }
            telemetry["payload_blocks"] =
                (1 + pack.callers.len() + pack.callees.len()).into();
            telemetry["max_context_blocks"] = max_blocks.into();
            inject_response_telemetry(
                &mut telemetry,
                blocks_scanned,
                total_time_ms,
                1 + pack.callers.len() + pack.callees.len(),
            );
            let t_cl = Instant::now();
            let clusters = cluster_infos_from_scoped(scoped);
            prof.push(("clusters", t_cl.elapsed().as_secs_f64() * 1000.0));
            let t_br = Instant::now();
            let mut bridges = bridge_infos(graph, scoped, root_path, 12);
            // Prefer bridges that touch the target's cluster
            if let Some(ac) = target_info.cluster.as_ref() {
                bridges.sort_by_key(|b| {
                    let touch = (b.from_cluster == *ac || b.to_cluster == *ac) as i32;
                    -touch
                });
            }
            prof.push(("bridges", t_br.elapsed().as_secs_f64() * 1000.0));
            // Suggest scopes for active cluster when blank scope; merge caller dir facets (P1).
            let t_sug = Instant::now();
            let mut suggested_scopes = vec![];
            if crate::server::filters::is_blank_scope(&req.scope_paths) {
                if let Some(sum) = code_graph::summarize_clusters(std::iter::once(target))
                    .into_iter()
                    .next()
                {
                    suggested_scopes = code_graph::suggested_scopes_for_cluster(&sum)
                        .into_iter()
                        .filter_map(|s| sanitize_scope_prefix(root_path, &s))
                        .collect();
                    if suggested_scopes.is_empty() {
                        suggested_scopes = suggested_scopes_from_paths(
                            root_path,
                            std::iter::once(target.file.to_string_lossy().as_ref()),
                            3,
                        );
                    }
                }
            }
            // Pin mitigation: facets from reverse callers when sample omits.
            // Only keep sanitizer-approved repo-relative pins (never host paths).
            if callers_omitted > 0 || window_meta.sample_window_exhausted {
                for f in &caller_dir_facets {
                    let Some(pin) = sanitize_scope_prefix(root_path, f) else {
                        continue;
                    };
                    if !pin.is_empty() && !suggested_scopes.iter().any(|s| s == &pin) {
                        suggested_scopes.push(pin);
                    }
                    if suggested_scopes.len() >= 8 {
                        break;
                    }
                }
            }
            prof.push(("suggest_scopes", t_sug.elapsed().as_secs_f64() * 1000.0));

            // Mermaid uses the *same* packed L1 set as dense/structured (dossier).
            let t_mm = Instant::now();
            trace_mermaid = Some(crate::server::render::build_trace_mermaid_packed(
                target,
                &pack.callers,
                &pack.callees,
                callers_omitted,
                callees_omitted,
            ));
            prof.push(("mermaid", t_mm.elapsed().as_secs_f64() * 1000.0));

            let t_asm = Instant::now();
            let seed_has_edges = !pack.callers.is_empty()
                || !pack.callees.is_empty()
                || !bridge_callers.is_empty()
                || !bridge_callees.is_empty()
                || graph.file_has_edges(&target.file);
            let conf = build_status::confidence_rung(
                state,
                root,
                Some(graph),
                true, // Trace seed came from index/exact path
                seed_has_edges,
            );
            telemetry["confidence"] = conf.as_str().into();
            telemetry["percent"] = bg_pct.into();
            telemetry["edges_complete"] = edges_complete.into();

            // Reverse CALL spine (compact edit path) — CALL only, bounded.
            // Hints = pack hop-1 (incl. loc-fallback when warehouse reverse empty).
            let caller_path = spine::reverse_call_spine(
                graph,
                target,
                root_path,
                pp,
                &pack.callers,
            );
            structured = Some(StructuredReport {
                state: make_state_info(
                    format!("{}% | {}", bg_pct, bg_status),
                    jit_note.to_string(),
                    conf,
                    bg_pct,
                ),
                error: None,
                target: Some(target_info),
                callers: pack.callers,
                callees: pack.callees,
                caller_path,
                peer_callers,
                bridge_callers,
                bridge_callees,
                blast_domain: Some(blast_domain.to_string()),
                seed_kind: Some(target.kind.clone()),
                receipt: None,
                next_action: None,
                telemetry,
                suggested_scopes,
                skeleton: None,
                hubs: None,
                module_resolved_from,
                module_interior_candidates,
                locations: if locations.is_empty() {
                    None
                } else {
                    Some(locations)
                },
                clusters: if clusters.is_empty() {
                    None
                } else {
                    Some(clusters)
                },
                bridges: if bridges.is_empty() {
                    None
                } else {
                    Some(bridges)
                },
                active_cluster: Some(t_cluster),
            });
            // Hub / Soft I4 window: concrete next re-pull (offset / pin facet / diverse).
            if let Some(ref mut st) = structured {
                let blank = crate::server::filters::is_blank_scope(&req.scope_paths);
                let window_next = crate::server::trace_pack::sample_window_next_action(
                    callers_omitted,
                    window_meta.sample_offset,
                    st.callers.len(),
                    window_meta.sample_mode,
                    &caller_dir_facets,
                    blank,
                    window_meta.sample_window_exhausted,
                );
                if let Some(n) = window_next {
                    set_next_action(st, n);
                } else if seed_in_degree >= HUB_FANIN_NEXT {
                    set_next_action(
                        st,
                        next_action_mega_hub(
                            seed_in_degree,
                            blank,
                            detail_mode.is_long(),
                        ),
                    );
                }
            }
            prof.push(("post_mermaid", t_asm.elapsed().as_secs_f64() * 1000.0));

            // Cold-Trace sniper: always emit one summary (grep TRACE_PROFILE).
            // Phases with ms; total includes seed→pack (not lobby/JIT outside).
            {
                let total_ms = t_trace.elapsed().as_secs_f64() * 1000.0;
                let named_sum: f64 = prof.iter().map(|(_, ms)| *ms).sum();
                prof.push(("total_seed_to_pack", total_ms));
                // Residual = time inside island not covered by named phases (bug/fault).
                prof.push(("unaccounted", (total_ms - named_sum).max(0.0)));
                let parts: Vec<String> = prof
                    .iter()
                    .map(|(k, ms)| format!("{k}={ms:.1}ms"))
                    .collect();
                vprintln!(
                    "⏱️  TRACE_PROFILE symbol={:?} visited={} fan_out_pruned={} | {}",
                    symbol,
                    trace_stats.nodes_visited,
                    trace_stats.fan_out_pruned,
                    parts.join(" ")
                );
            }

            // Remember the tour when the building is stable (graph epoch).
            if let Some(ref rep) = structured {
                if let Some(payload) = crate::server::trace_memo::payload_from_report(
                    goal_str,
                    symbol.trim(),
                    memo_epoch,
                    target.id.as_str(),
                    rep,
                    callers_omitted,
                    callees_omitted,
                ) {
                    crate::server::trace_memo::store(Path::new(root), memo_key, payload);
                    vprintln!(
                        "💾 Trace memo STORE key={:#x} epoch={:#x} symbol={:?}",
                        memo_key,
                        memo_epoch,
                        symbol.trim()
                    );
                }
            }

            // Human summary in content; full report in structured.
            } // end !needs_homonym_disambiguation (full Trace)
        } // end if let Some(target)
        } // end !memo_hit
    }

    (structured, trace_mermaid)
}
