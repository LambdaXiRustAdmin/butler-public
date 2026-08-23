//! Orchestrate unit tests (peeled from mod.rs — behavior-neutral P1a).
//! Lives under `#[cfg(test)] mod detail_tests`.

use super::*;
use super::receipt::why_edge_for;
use super::render::{
    compact_headline, neighbor_basis_tag, orchestrate_content_compact,
    orchestrate_content_dense, orchestrate_content_arch_compact,
};
use crate::server::dto::{CallerCallee, StateInfo, StructuredReport, SymbolLocation, TargetInfo};

fn sample_trace() -> StructuredReport {
    let mut st = StructuredReport {
        state: StateInfo {
            edge_build: "100% | Complete".into(),
            jit: "None".into(),
            confidence: Some("edges_full".into()),
            percent: Some(100),
        },
        error: None,
        target: Some(TargetInfo {
            name: "load_weights".into(),
            file: "/repo/code_graph/src/gnn/projection.rs".into(),
            line: 154,
            definition: Some("pub fn load_weights() {}".into()),
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
        }),
        callers: vec![],
        callees: vec![CallerCallee {
            name: "parse_f32_le".into(),
            file: "/repo/code_graph/src/gnn/projection.rs".into(),
            line: 93,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
        cite: None,
        why: None,
        }],
        caller_path: vec![],
        peer_callers: vec![],
        bridge_callers: vec![],
        bridge_callees: vec![],
        blast_domain: Some("call".into()),
        seed_kind: Some("function_item".into()),
        receipt: None,
        next_action: None,
        telemetry: serde_json::json!({ "payload_blocks": 2, "edges_complete": true }),
        suggested_scopes: vec![],
        skeleton: None,
        hubs: None,
        module_resolved_from: None,
        module_interior_candidates: None,
        locations: None,
        clusters: None,
        bridges: None,
        active_cluster: Some("core:rs".into()),
    };
    attach_trace_receipt(&mut st);
    st
}

#[test]
fn compact_headline_partial_says_so_far_not_dead() {
    let mut st = sample_trace();
    st.state.confidence = Some("index_exact".into());
    st.state.percent = Some(15);
    st.state.edge_build = "15% | Mapping".into();
    st.callers.clear();
    st.callees.clear();
    attach_trace_receipt(&mut st);
    let h = compact_headline(&st);
    assert!(h.contains("so far"), "{h}");
    assert!(h.contains("partial 15%"), "{h}");
    assert!(h.contains("rewalk"), "{h}");
    assert!(h.contains("receipt:"), "{h}");
    let empty = empty_callers_line(st.target.as_ref().unwrap(), &st);
    assert!(empty.contains("0 so far"), "{empty}");
    assert!(empty.contains("dead code"), "{empty}");
}

#[test]
fn empty_callers_line_zero_call_not_dead_code() {
    let mut st = sample_trace();
    st.callers.clear();
    st.callees.clear();
    st.telemetry = serde_json::json!({
        "seed_in_degree": 0,
        "seed_out_degree": 0,
        "edges_complete": true,
    });
    st.state.confidence = Some("edges_full".into());
    st.state.percent = Some(100);
    let empty = empty_callers_line(st.target.as_ref().unwrap(), &st);
    assert!(
        empty.contains("not unused")
            || empty.contains("not proof of dead code")
            || empty.contains("not dead code"),
        "{empty}"
    );
    assert!(
        empty.contains("CALL") || empty.contains("callback") || empty.contains("direct_callers"),
        "{empty}"
    );
}

#[test]
fn compact_renders_reverse_call_spine_when_present() {
    let mut st = sample_trace();
    st.caller_path = vec![CallerCallee {
        name: "dispatch_tool".into(),
        file: "cli/src/server/tools.rs".into(),
        line: 42,
        hop: 1,
        lang: Some("rust".into()),
        cluster: Some("core:rs".into()),
        relation: None,
        cite: None,
        why: None,
    }];
    attach_trace_receipt(&mut st);
    let body = orchestrate_content_compact(&st);
    assert!(
        body.contains("call path (reverse spine · CALL only)"),
        "{body}"
    );
    assert!(body.contains("load_weights"), "{body}");
    assert!(body.contains("<- dispatch_tool @"), "{body}");
    assert!(body.contains("tools.rs:42"), "{body}");
}

#[test]
fn compact_omits_spine_section_when_empty() {
    let st = sample_trace();
    let body = orchestrate_content_compact(&st);
    assert!(
        !body.contains("call path (reverse spine"),
        "empty path must stay silent: {body}"
    );
}

#[test]
fn receipt_high_complete_bare_name_with_call_neighbors() {
    let st = sample_trace();
    let r = st.receipt.as_ref().expect("receipt attached");
    assert_eq!(r.confidence, "high");
    assert_eq!(r.ladder, "edges_full");
    assert_eq!(r.basis, "bare-name");
    assert_eq!(r.edges, "complete");
    let h = compact_headline(&st);
    assert!(h.contains("receipt: high | bare-name | complete"), "{h}");
}

#[test]
fn compact_headline_seed_path_line_before_census_not_file_colon_count() {
    // Glove-fit: `file.rs: 9 direct…` was misread as line 9. Always path:line · census.
    let mut st = sample_trace();
    st.target.as_mut().unwrap().line = 213;
    st.target.as_mut().unwrap().file =
        "/repo/cli/src/harvester/template.rs".into();
    st.callers = (0..9)
        .map(|i| CallerCallee {
            name: format!("caller_{i}"),
            file: format!("other_{i}.rs"),
            line: i + 1,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
            cite: None,
            why: None,
        })
        .collect();
    // Nine hop-2 so census can start with a digit after a colon if we regress.
    st.callers.extend((0..9).map(|i| CallerCallee {
        name: format!("hop2_{i}"),
        file: format!("far_{i}.rs"),
        line: 100 + i,
        hop: 2,
        lang: Some("rust".into()),
        cluster: Some("core:rs".into()),
        relation: None,
        cite: None,
        why: None,
    }));
    attach_trace_receipt(&mut st);
    let h = compact_headline(&st);
    assert!(
        h.contains("template.rs:213"),
        "seed line must appear as path:line: {h}"
    );
    assert!(
        h.contains(" · "),
        "middle-dot must separate seed locus from census: {h}"
    );
    // Forbidden pattern: path ending then colon-space-digit (old census trap).
    assert!(
        !h.contains("template.rs: 9") && !h.contains("template.rs:9 direct"),
        "must not look like line 9 when census is 9 direct: {h}"
    );
    assert!(
        h.contains("9 direct+9 hop≥2 CALL callers") || h.contains("direct+"),
        "census still present: {h}"
    );
}

#[test]
fn receipt_type_neighborhood_basis() {
    let mut st = sample_trace();
    st.blast_domain = Some("type_neighborhood".into());
    attach_trace_receipt(&mut st);
    assert_eq!(
        st.receipt.as_ref().map(|r| r.basis.as_str()),
        Some("type-neighborhood")
    );
}

#[test]
fn receipt_location_only_when_no_neighbors() {
    let mut st = sample_trace();
    st.callers.clear();
    st.callees.clear();
    st.bridge_callers.clear();
    st.bridge_callees.clear();
    attach_trace_receipt(&mut st);
    assert_eq!(
        st.receipt.as_ref().map(|r| r.basis.as_str()),
        Some("location-only")
    );
}

#[test]
fn receipt_bridge_export_basis() {
    let mut st = sample_trace();
    st.callers.clear();
    st.callees.clear();
    st.bridge_callers.push(CallerCallee {
        name: "search_py".into(),
        file: "word_count.py".into(),
        line: 1,
        hop: 1,
        lang: Some("python".into()),
        cluster: Some("shell:py".into()),
        relation: Some("export".into()),
        cite: None,
        why: None,
    });
    attach_trace_receipt(&mut st);
    assert_eq!(
        st.receipt.as_ref().map(|r| r.basis.as_str()),
        Some("bridge-export")
    );
}

#[test]
fn t2_homonym_risk_names() {
    assert!(is_homonym_risk_name("build"));
    assert!(is_homonym_risk_name("run"));
    assert!(is_homonym_risk_name("main"));
    assert!(is_homonym_risk_name("App"));
    assert!(!is_homonym_risk_name("createServer"));
    assert!(!is_homonym_risk_name("LoadingButton"));
    assert!(!is_homonym_risk_name("foo::Bar"));
}

#[test]
fn t2_needs_disambiguation_on_many_files() {
    let locs = vec![
        SymbolLocation {
            name: "build".into(),
            file: "src/node/build.ts".into(),
            line: 1,
            end_line: None,
            kind: "function_declaration".into(),
            preferred: true,
            lang: Some("typescript".into()),
            cluster: None,
        },
        SymbolLocation {
            name: "build".into(),
            file: "src/node/optimizer/index.ts".into(),
            line: 1,
            end_line: None,
            kind: "function_declaration".into(),
            preferred: false,
            lang: Some("typescript".into()),
            cluster: None,
        },
        SymbolLocation {
            name: "build".into(),
            file: "src/node/cli.ts".into(),
            line: 1,
            end_line: None,
            kind: "function_declaration".into(),
            preferred: false,
            lang: Some("typescript".into()),
            cluster: None,
        },
    ];
    assert!(needs_homonym_disambiguation("build", &locs, None));
    assert!(!needs_homonym_disambiguation(
        "build",
        &locs,
        Some(&["src/node/build.ts".into()])
    ));
    assert!(!needs_homonym_disambiguation("createServer", &locs, None));
    // Dual-lang twins (ts+rs): 2 files enough for short names.
    let dual = vec![
        SymbolLocation {
            name: "echo".into(),
            file: "src/views/Communication.svelte".into(),
            line: 39,
            end_line: None,
            kind: "function_declaration".into(),
            preferred: false,
            lang: Some("typescript".into()),
            cluster: None,
        },
        SymbolLocation {
            name: "echo".into(),
            file: "src-tauri/src/cmd.rs".into(),
            line: 52,
            end_line: None,
            kind: "function_item".into(),
            preferred: false,
            lang: Some("rust".into()),
            cluster: None,
        },
    ];
    assert!(
        needs_homonym_disambiguation("echo", &dual, None),
        "ts+rs echo must disambiguate at 2 files"
    );
    // Collision multi-file (lets only — no serious defs): still force T.2 (bevy `app`).
    let lets = vec![
        SymbolLocation {
            name: "app".into(),
            file: "crates/a/src/lib.rs".into(),
            line: 1,
            end_line: None,
            kind: "let_declaration".into(),
            preferred: false,
            lang: Some("rust".into()),
            cluster: None,
        },
        SymbolLocation {
            name: "app".into(),
            file: "crates/b/src/lib.rs".into(),
            line: 1,
            end_line: None,
            kind: "let_declaration".into(),
            preferred: false,
            lang: Some("rust".into()),
            cluster: None,
        },
        SymbolLocation {
            name: "app".into(),
            file: "crates/c/src/mod.rs".into(),
            line: 1,
            end_line: None,
            kind: "mod_item".into(),
            preferred: false,
            lang: Some("rust".into()),
            cluster: None,
        },
    ];
    assert!(
        needs_homonym_disambiguation("app", &lets, None),
        "multi-file let/mod collision must disambiguate danger names"
    );
    assert!(
        needs_homonym_disambiguation("invoke", &lets, None),
        "invoke is danger — multi-file collision disambiguates"
    );
}

#[test]
fn t2_compact_headline_disambiguate() {
    let mut st = sample_trace();
    st.blast_domain = Some("disambiguate".into());
    st.error = Some("disambiguate: 'build' has 3 serious…".into());
    st.callers.clear();
    st.callees.clear();
    st.telemetry = serde_json::json!({
        "disambiguate": true,
        "serious_alt_files": 3,
        "edges_complete": true,
    });
    st.state.confidence = Some("index_exact".into());
    attach_trace_receipt(&mut st);
    let h = compact_headline(&st);
    assert!(h.contains("Disambiguate"), "{h}");
    assert!(h.contains("serious"), "{h}");
    assert!(h.contains("receipt:"), "{h}");
    assert!(h.contains("next:"), "disambiguate must tutor-copy next: {h}");
    assert_eq!(
        st.receipt.as_ref().map(|r| r.basis.as_str()),
        Some("disambiguate")
    );
}

#[test]
fn symbol_not_found_provisional_when_incomplete() {
    let msg = symbol_not_found_message("App", &None, false, 15);
    assert!(msg.contains("symbol_not_seen_yet@15%"), "{msg}");
    assert!(!msg.contains("no_structural_hits"), "{msg}");
    assert!(msg.contains("next:"), "{msg}");
    assert!(msg.contains("retry"), "{msg}");
    let full = symbol_not_found_message("App", &None, true, 100);
    assert!(!full.contains("symbol_not_seen_yet"), "{full}");
    assert!(full.contains("next:"), "{full}");
    assert!(
        full.contains("scope_paths") || full.contains("collision"),
        "{full}"
    );
}

#[test]
fn t3_next_action_homonym_vs_long_name() {
    let short = next_action_symbol_miss("build", true, 100);
    assert!(short.contains("scope_paths"), "{short}");
    assert!(short.contains("collide") || short.contains("collision") || short.contains("short"), "{short}");
    let long = next_action_symbol_miss("load_weights_from_path", true, 100);
    assert!(long.contains("scope_paths") || long.contains("spelling"), "{long}");
    assert!(!long.contains("collide"), "{long}");
    let partial = next_action_symbol_miss("build", false, 22);
    assert!(partial.contains("22%"), "{partial}");
    assert!(partial.contains("retry"), "{partial}");
}

#[test]
fn t3_miss_report_sets_next_action_field() {
    let mut st = error_structured_report(
        &symbol_not_found_message("build", &None, true, 100),
        "100% | Complete",
        "None",
        build_status::ConfidenceRung::EdgesFull,
        100,
    );
    set_next_action(&mut st, next_action_symbol_miss("build", true, 100));
    assert!(st.next_action.as_ref().is_some_and(|n| n.contains("scope_paths")));
    assert_eq!(
        st.telemetry.get("next_action").and_then(|v| v.as_str()),
        st.next_action.as_deref()
    );
    let h = compact_headline(&st);
    assert!(h.contains("next:"), "{h}");
    let dense = orchestrate_content_dense(&st, None);
    assert!(dense.contains("next:"), "{dense}");
}

#[test]
fn t1c_why_edge_bridge_and_transitive_silence_bare_call() {
    let bare = CallerCallee {
        name: "helper".into(),
        file: "a.rs".into(),
        line: 1,
        hop: 1,
        lang: Some("rust".into()),
        cluster: None,
        relation: None,
        cite: None,
        why: None,
    };
    assert!(why_edge_for("seed", "→", &bare).is_none());
    let hop2 = CallerCallee {
        hop: 2,
        ..bare.clone()
    };
    let w = why_edge_for("seed", "→", &hop2).expect("transitive why");
    assert!(w.contains("transitive hop 2"), "{w}");
    let export = CallerCallee {
        name: "search_py".into(),
        file: "w.py".into(),
        line: 3,
        hop: 1,
        lang: Some("python".into()),
        cluster: None,
        relation: Some("export".into()),
        cite: None,
        why: None,
    };
    let w = why_edge_for("search", "→", &export).expect("export why");
    assert!(w.contains("export bridge"), "{w}");
}

#[test]
fn detail_from_req_defaults_compact() {
    assert_eq!(ContentDetail::from_req(None), ContentDetail::Compact);
    assert_eq!(ContentDetail::from_req(Some("compact")), ContentDetail::Compact);
    assert_eq!(ContentDetail::from_req(Some("short")), ContentDetail::Compact);
    assert_eq!(ContentDetail::from_req(Some("DENSE")), ContentDetail::Dense);
    assert_eq!(ContentDetail::from_req(Some("long")), ContentDetail::Dense);
    assert_eq!(ContentDetail::from_req(Some("full")), ContentDetail::Dense);
    assert!(!ContentDetail::Compact.is_long());
    assert!(ContentDetail::Dense.is_long());
    assert_eq!(ContentDetail::Compact.as_length_label(), "short");
    assert_eq!(ContentDetail::Dense.as_length_label(), "long");
}

#[test]
fn empty_scope_routing_bevy_src_not_warehouse_too_broad() {
    // Live dogfood: file_hits=1, est≈121, warehouse=141611 → must NOT be "too broad".
    assert!(
        !scope_working_set_truly_too_big(1, 121, 141_611),
        "bevy root src/ (1 file) must take empty-blocks repair"
    );
    assert!(!scope_working_set_truly_too_big(0, 0, 141_611));
    // Fat scopes still refuse.
    assert!(scope_working_set_truly_too_big(500, 90_000, 400_000));
    assert!(scope_working_set_truly_too_big(2_500, 10_000, 50_000));
    assert!(scope_working_set_truly_too_big(500, 10_000, 141_611));
}

#[test]
fn suggested_scopes_never_emit_host_home_prefix() {
    let root = Path::new("/projects/test_repos/example");
    // Synthetic host-absolute display form (generic — not a real machine path).
    let leaked = "/home/user/projects/test_repos/example/include/foo/attr.h";
    let from_host = sanitize_scope_prefix(root, leaked);
    assert!(
        from_host.as_ref().map(|s| !s.starts_with("home/")).unwrap_or(true),
        "host display path must not become home/… scopes: {from_host:?}"
    );
    // Repo-relative stays useful.
    let ok = sanitize_scope_prefix(root, "include/foo/attr.h").unwrap();
    assert_eq!(ok, "include/foo/");
    let scopes = suggested_scopes_from_paths(
        root,
        [leaked, "include/foo/numpy.h", "tests/test_foo.py"],
        3,
    );
    assert!(
        scopes.iter().all(|s| !s.starts_with("home/") && !s.contains("/home/")),
        "scopes={scopes:?}"
    );
    assert!(scopes.iter().any(|s| s.starts_with("include/") || s.starts_with("tests/")));
}

#[test]
fn t2_suggested_scopes_from_locations_are_repo_relative_pins() {
    // Locations often carry to_display host paths; pins must be copy-paste safe.
    let root = Path::new("/projects/test_repos/gin");
    let locs = vec![SymbolLocation {
        name: "Default".into(),
        file: "/home/user/projects/test_repos/gin/gin.go".into(),
        line: 1,
        end_line: None,
        kind: "function_declaration".into(),
        preferred: true,
        lang: Some("go".into()),
        cluster: None,
    }];
    let refs: Vec<&SymbolLocation> = locs.iter().collect();
    let scopes = suggested_scopes_from_locations(root, &refs, 8);
    assert!(
        !scopes.is_empty(),
        "expected at least one pin from gin.go location"
    );
    assert!(
        scopes
            .iter()
            .all(|s| !s.starts_with('/') && !s.contains("/home/") && !s.starts_with("home/")),
        "scopes must be repo-relative, got {scopes:?}"
    );
    assert!(
        scopes.iter().any(|s| s == "gin.go" || s.ends_with("gin.go")),
        "file pin gin.go expected, got {scopes:?}"
    );
}

#[test]
fn arch_compact_map_lists_skeleton_hubs_and_next() {
    let mut st = sample_trace();
    st.target = None;
    st.callers.clear();
    st.callees.clear();
    st.skeleton = Some(vec![
        "cli/src/server/context_engine.rs".into(),
        "cli/src/server/orchestrate/mod.rs".into(),
        "cli/src/server/filters/mod.rs".into(),
        "cli/src/server/filters/homonym.rs".into(),
    ]);
    st.hubs = Some(vec![crate::server::dto::Hub {
        name: "handle_orchestrate".into(),
        file: "cli/src/server/orchestrate/mod.rs".into(),
        score: 9.0,
        lang: Some("rust".into()),
        cluster: Some("core:rs".into()),
    }]);
    st.suggested_scopes = vec!["cli/src/server/".into()];
    st.next_action = None;
    st.telemetry = serde_json::json!({
        "type": "architectural",
        "coverage_complete": true,
        "unique_files_under_scope": 4,
        "payload_omitted": 0,
        "skeleton_rolled_up": false,
    });
    let c = orchestrate_content_summary(Some(&st), None, ContentDetail::Compact);
    assert!(c.contains("tree"), "want tree: {c}");
    assert!(c.contains("coverage:") && c.contains("complete"), "{c}");
    assert!(c.contains("context_engine") || c.contains("orchestrate"), "{c}");
    assert!(c.contains("hubs"), "{c}");
    assert!(c.contains("handle_orchestrate"), "{c}");
    assert!(c.contains("suggested_scopes"), "{c}");
    assert!(c.contains("next:"), "arch compact must tutor next: {c}");
    assert!(c.contains("list_dir") || c.contains("format this map"), "{c}");
    let direct = orchestrate_content_arch_compact(&st);
    assert!(direct.lines().count() > 5, "{direct}");
}

#[test]
fn compact_splits_external_and_same_file_callers() {
    use super::render::{is_trace_neighbor_noise_name, paths_same_file};
    assert!(paths_same_file(
        "/repo/code_graph/src/gnn/projection.rs",
        "code_graph/src/gnn/projection.rs"
    ));
    assert!(!paths_same_file(
        "code_graph/src/gnn/projection.rs",
        "code_graph/src/gnn/other.rs"
    ));
    assert!(is_trace_neighbor_noise_name("fmt"));
    assert!(is_trace_neighbor_noise_name("Debug"));
    assert!(!is_trace_neighbor_noise_name("createClient"));

    let mut st = sample_trace();
    st.callers = vec![
        CallerCallee {
            name: "fmt".into(),
            file: "/repo/code_graph/src/gnn/projection.rs".into(),
            line: 50,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
            cite: None,
            why: None,
        },
        CallerCallee {
            name: "local_helper".into(),
            file: "/repo/code_graph/src/gnn/projection.rs".into(),
            line: 200,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
            cite: None,
            why: None,
        },
        CallerCallee {
            name: "external_entry".into(),
            file: "/repo/cli/src/server/handlers.rs".into(),
            line: 10,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
            cite: None,
            why: None,
        },
    ];
    let compact = orchestrate_content_summary(Some(&st), None, ContentDetail::Compact);
    assert!(
        compact.contains("external callers") && compact.contains("local helpers"),
        "want glove-fit sections: {compact}"
    );
    assert!(compact.contains("external_entry"), "{compact}");
    assert!(compact.contains("local_helper"), "{compact}");
    // External appears before local/noise in content.
    let ext_i = compact.find("external_entry").expect("ext");
    let loc_i = compact.find("local_helper").expect("loc");
    let noise_i = compact.find("`fmt`").expect("noise fmt");
    assert!(
        ext_i < loc_i && loc_i < noise_i,
        "order external < local < noise: {compact}"
    );
    assert!(
        compact.contains("trait/boilerplate noise") || compact.contains("noise"),
        "{compact}"
    );
    let helper_line = compact
        .lines()
        .find(|l: &&str| l.contains("local_helper"))
        .unwrap_or("");
    assert!(
        helper_line.contains("same-file"),
        "helper must be tagged: {helper_line}"
    );
}

#[test]
fn compact_all_external_still_labels_external_header() {
    // handle_orchestrate post-v29: only cross-file callers — still glove-fit headers.
    let mut st = sample_trace();
    st.callers = vec![CallerCallee {
        name: "dispatch_tool".into(),
        file: "/repo/cli/src/server/context_engine.rs".into(),
        line: 1559,
        hop: 1,
        lang: Some("rust".into()),
        cluster: Some("core:rs".into()),
        relation: None,
        cite: None,
        why: None,
    }];
    st.callees.clear();
    let compact = orchestrate_content_summary(Some(&st), None, ContentDetail::Compact);
    assert!(
        compact.contains("all external") || compact.contains("external callers"),
        "all-external list must keep loud external header: {compact}"
    );
    assert!(compact.contains("dispatch_tool"), "{compact}");
    assert!(
        compact.contains("primary edit targets") || compact.contains("external callers"),
        "{compact}"
    );
    assert!(!compact.contains("all same-file helpers"), "{compact}");
}

#[test]
fn arch_compact_rollup_marks_incomplete_and_tutors_rearch() {
    let mut st = sample_trace();
    st.target = None;
    st.callers.clear();
    st.callees.clear();
    st.skeleton = Some(vec![
        "rich/__main__.py".into(),
        "rich/ (76 files)".into(),
        "rich/_unicode_data/ (23 files)".into(),
    ]);
    st.hubs = Some(vec![crate::server::dto::Hub {
        name: "Console".into(),
        file: "rich/console.py".into(),
        score: 9.0,
        lang: Some("python".into()),
        cluster: Some("shell:py".into()),
    }]);
    st.suggested_scopes = vec!["rich/".into()];
    st.next_action = Some(
        "skeleton rolled up — re-Arch with scope_paths on a listed directory for full basenames; prefer suggested_scopes over list_dir"
            .into(),
    );
    st.telemetry = serde_json::json!({
        "type": "architectural",
        "coverage_complete": false,
        "skeleton_rolled_up": true,
        "unique_files_under_scope": 100,
        "payload_omitted": 0,
    });
    let c = orchestrate_content_arch_compact(&st);
    assert!(c.contains("incomplete") || c.contains("rolled"), "{c}");
    assert!(!c.contains("(complete)"), "must not claim complete on rollup: {c}");
    assert!(
        c.contains("re-Arch") || c.contains("scope_paths") || c.contains("suggested_scopes"),
        "want escape hatch next: {c}"
    );
}

#[test]
fn format_skeleton_tree_indents_nested() {
    use super::render::format_skeleton_tree;
    let paths = vec![
        "cli/src/server/mod.rs".into(),
        "cli/src/server/dto.rs".into(),
        "cli/src/server/filters/mod.rs".into(),
        "cli/src/server/filters/homonym.rs".into(),
        "cli/src/server/orchestrate/mod.rs".into(),
    ];
    let t = format_skeleton_tree(&paths, 20);
    let joined = t.join("\n");
    // Directory nodes — filters/ is not a child of dto.rs
    assert!(
        joined.contains("filters/"),
        "want explicit filters/ dir line: {joined}"
    );
    assert!(
        joined.contains("orchestrate/") || joined.contains("orchestrate"),
        "{joined}"
    );
    let dto_i = joined.find("dto.rs");
    let filters_i = joined.find("filters/");
    assert!(dto_i.is_some() && filters_i.is_some(), "{joined}");
    // filters/ line should not be more-indented under dto as a fake child of a file
    let lines: Vec<&str> = t.iter().map(|s| s.as_str()).collect();
    let dto_line = lines.iter().find(|l| l.contains("dto.rs")).copied();
    let filters_line = lines.iter().find(|l| l.contains("filters/")).copied();
    if let (Some(d), Some(f)) = (dto_line, filters_line) {
        let d_indent = d.len() - d.trim_start().len();
        let f_indent = f.len() - f.trim_start().len();
        assert_eq!(
            d_indent, f_indent,
            "dto.rs and filters/ should be siblings, got:\n{joined}"
        );
    }
}

#[test]
fn compact_trust_dossier_has_receipt_basis_and_neighbors() {
    let st = sample_trace();
    let compact = orchestrate_content_summary(Some(&st), None, ContentDetail::Compact);
    // Compact is multi-line **text UI** — not silent structured-only trust.
    assert!(compact.contains('\n'), "compact trust dossier must be multi-line: {compact}");
    assert!(compact.contains("load_weights"));
    assert!(compact.contains('★'), "preferred seed marker: {compact}");
    assert!(
        compact.contains("receipt:"),
        "receipt must appear in content text: {compact}"
    );
    assert!(
        compact.contains("basis:") || compact.contains("bare-name"),
        "basis visible in content: {compact}"
    );
    assert!(
        compact.contains("parse_f32_le") && compact.contains("[basis:"),
        "neighbors must carry basis tags: {compact}"
    );
    // sample_trace callee is same file as seed — must label same-file (not external entry).
    assert!(
        compact.contains("same-file"),
        "same-file helpers must be tagged: {compact}"
    );
    assert!(
        compact.contains("next:"),
        "next_action line required in compact: {compact}"
    );
    assert!(
        compact.contains("bridges:"),
        "bridges section always present (even if none): {compact}"
    );

    // Bridge-only neighborhood must still end with next:
    let mut br = sample_trace();
    br.callers.clear();
    br.callees.clear();
    br.bridge_callees = vec![CallerCallee {
        name: "search".into(),
        file: "src/lib.rs".into(),
        line: 5,
        hop: 1,
        lang: Some("rust".into()),
        cluster: None,
        relation: Some("export".into()),
        cite: None,
        why: Some("search_py → search via export bridge".into()),
    }];
    attach_trace_receipt(&mut br);
    let c2 = orchestrate_content_summary(Some(&br), None, ContentDetail::Compact);
    assert!(c2.contains("basis: export"), "{c2}");
    assert!(c2.contains("next:"), "bridge-only compact must have next: {c2}");

    let dense =
        orchestrate_content_summary(Some(&st), Some("graph LR"), ContentDetail::Dense);
    assert!(dense.contains("definition:"));
    assert!(dense.contains("parse_f32_le"));
    assert!(dense.contains("mermaid:"));
    assert!(dense.contains("core:rs"), "lang cluster badge on target/callees: {dense}");
    assert!(dense.contains("active_cluster: core:rs"));
}

#[test]
fn neighbor_basis_tags_are_honest() {
    let call = CallerCallee {
        name: "a".into(),
        file: "a.rs".into(),
        line: 1,
        hop: 1,
        lang: None,
        cluster: None,
        relation: None,
        cite: None,
        why: None,
    };
    assert_eq!(neighbor_basis_tag(&call), "call");
    let hop2 = CallerCallee {
        hop: 2,
        ..call.clone()
    };
    assert_eq!(neighbor_basis_tag(&hop2), "transitive");
    let exp = CallerCallee {
        relation: Some("export".into()),
        hop: 1,
        ..call.clone()
    };
    assert_eq!(neighbor_basis_tag(&exp), "export");
    let peer = CallerCallee {
        relation: Some("name_peer".into()),
        hop: 1,
        ..call
    };
    assert_eq!(neighbor_basis_tag(&peer), "name_peer");
}

#[test]
fn compact_segregates_peer_callers_from_hard_call() {
    let mut st = sample_trace();
    st.callers = vec![CallerCallee {
        name: "DirectCaller".into(),
        file: "pkg/a.rs".into(),
        line: 10,
        hop: 1,
        lang: Some("rust".into()),
        cluster: Some("core:rs".into()),
        relation: None,
        cite: None,
        why: None,
    }];
    st.peer_callers = vec![CallerCallee {
        name: "PeerOnlyCaller".into(),
        file: "pkg/b.rs".into(),
        line: 20,
        hop: 1,
        lang: Some("rust".into()),
        cluster: Some("core:rs".into()),
        relation: Some("name_peer".into()),
        cite: None,
        why: Some(
            "calls same-name peer `DefaultOptions` @ other/db.go — not a CALL into the ★ pin"
                .into(),
        ),
    }];
    st.telemetry = serde_json::json!({
        "seed_in_degree": 1,
        "seed_out_degree": 1,
        "seed_in_degree_name_peers": 1,
        "edges_complete": true,
        "payload_blocks": 3,
    });
    let body = orchestrate_content_compact(&st);
    assert!(
        body.contains("DirectCaller"),
        "hard CALL must stay in callers: {body}"
    );
    assert!(
        body.contains("peer_callers") && body.contains("PeerOnlyCaller"),
        "peer must be labeled section: {body}"
    );
    assert!(
        body.contains("not CALL into ★") || body.contains("name_peer"),
        "peer honesty tag required: {body}"
    );
    // Peer must not be the only story under unlabeled callers list
    let callers_idx = body.find("callers").unwrap_or(0);
    let peer_idx = body.find("peer_callers").unwrap_or(usize::MAX);
    assert!(
        peer_idx > callers_idx,
        "peer_callers section after callers: {body}"
    );
}

#[test]
fn scope_frame_mentions_peer_callers_separately() {
    let with_peers = scope_frame_line_with_peers(4, 2, 12, 0, false, true, 0);
    assert!(with_peers.contains("called by 4"), "{with_peers}");
    assert!(
        with_peers.contains("peer") && with_peers.contains("12"),
        "peer count labeled: {with_peers}"
    );
    assert!(
        with_peers.contains("not CALL into ★"),
        "peer honesty: {with_peers}"
    );
}

#[test]
fn compact_headline_reports_alt_location_count() {
    let mut st = sample_trace();
    st.locations = Some(vec![
        crate::server::dto::SymbolLocation {
            name: "load_weights".into(),
            file: "code_graph/src/gnn/projection.rs".into(),
            line: 154,
            end_line: None,
            kind: "function_item".into(),
            preferred: true,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
        },
        crate::server::dto::SymbolLocation {
            name: "load_weights".into(),
            file: "other/load_weights.rs".into(),
            line: 1,
            end_line: None,
            kind: "function_item".into(),
            preferred: false,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
        },
    ]);
    let h = compact_headline(&st);
    assert!(h.contains("1 alt locations"), "{h}");
    assert!(h.contains('★'), "{h}");
}

#[test]
fn dense_callee_marks_export_relation() {
    let mut st = sample_trace();
    // Bridges live in bridge_* lists (P.4), not CALL callees.
    st.bridge_callees = vec![CallerCallee {
        name: "search_py".into(),
        file: "word_count/__init__.py".into(),
        line: 4,
        hop: 1,
        lang: Some("python".into()),
        cluster: Some("shell:py".into()),
        relation: Some("export".into()),
    cite: None,
        why: None,
    }];
    let dense =
        orchestrate_content_summary(Some(&st), Some("graph LR"), ContentDetail::Dense);
    assert!(
        dense.contains("interconnect bridges") && dense.contains("export"),
        "typed bridge section must show export: {dense}"
    );
    assert!(
        dense.contains("domain: call"),
        "function seed must declare domain=call: {dense}"
    );
}

#[test]
fn dense_type_seed_warns_not_full_abi() {
    let mut st = sample_trace();
    st.blast_domain = Some("type_neighborhood".into());
    st.seed_kind = Some("struct_item".into());
    st.target.as_mut().unwrap().name = "PyObject".into();
    let dense =
        orchestrate_content_summary(Some(&st), None, ContentDetail::Dense);
    assert!(
        dense.contains("type_neighborhood") && dense.contains("NOT full ABI"),
        "type seed must warn about ABI limits: {dense}"
    );
    let h = compact_headline(&st);
    assert!(
        h.contains("type_neighborhood"),
        "compact must flag type domain: {h}"
    );
}

#[test]
fn scope_frame_frames_called_by_and_hub_language() {
    let narrow = scope_frame_line(2, 5, 0, false, true, 0);
    assert!(narrow.contains("called by 2"), "{narrow}");
    assert!(narrow.contains("calls 5"), "{narrow}");
    assert!(narrow.contains("narrow") || narrow.contains("moderate"), "{narrow}");
    let hub = scope_frame_line(120, 3, 0, false, true, 1);
    assert!(hub.contains("called by 120"), "{hub}");
    assert!(hub.contains("hub-scale") || hub.contains("shared infrastructure"), "{hub}");
    assert!(hub.contains("interconnect"), "{hub}");
    let capped = scope_frame_line(10, 10, 40, true, true, 0);
    assert!(capped.contains("lists capped") || capped.contains("complete"), "{capped}");
}

#[test]
fn compact_headline_splits_direct_vs_hop2_callees() {
    let mut st = sample_trace();
    st.callees = vec![
        CallerCallee {
            name: "direct_fn".into(),
            file: "a.rs".into(),
            line: 1,
            hop: 1,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
        cite: None,
        why: None,
        },
        CallerCallee {
            name: "transitive_fn".into(),
            file: "a.rs".into(),
            line: 2,
            hop: 2,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
        cite: None,
        why: None,
        },
        CallerCallee {
            name: "also_l2".into(),
            file: "a.rs".into(),
            line: 3,
            hop: 2,
            lang: Some("rust".into()),
            cluster: Some("core:rs".into()),
            relation: None,
        cite: None,
        why: None,
        },
    ];
    let h = compact_headline(&st);
    assert!(
        h.contains("1 direct+2 hop≥2 CALL callees"),
        "headline must not claim 3 direct callees: {h}"
    );
    assert!(
        h.contains("Before you edit") && h.contains("Hop-2 is not a caller"),
        "pain lead must name hop-2 as not a caller: {h}"
    );
    let dense = orchestrate_content_summary(Some(&st), None, ContentDetail::Dense);
    assert!(
        dense.contains("Hop-2 is not a caller") || dense.contains("hop>1 is not a direct call"),
        "dense header must warn hop-2 is not a caller: {dense}"
    );
    assert!(
        dense.contains("transitive_fn")
            && (dense.contains("hop=2") || dense.contains("hop≥2") || dense.contains("Hop-2")),
        "L2 must be labeled as hop-2 (census or row), not as extra directs: {dense}"
    );
    assert!(
        !dense.contains("direct_fn") || !dense.lines().any(|l| l.contains("direct_fn") && l.contains("hop=")),
        "hop=1 rows should not print hop=: {dense}"
    );
}

#[test]
fn cap_trace_payload_reserves_callee_slots() {
    let g = CodeGraph::new();
    let callers: Vec<CallerCallee> = (0..30)
        .map(|i| CallerCallee {
            name: format!("caller_{i}"),
            file: "a.c".into(),
            line: i + 1,
            hop: 1,
            lang: Some("c".into()),
            cluster: Some("core:c".into()),
            relation: None,
        cite: None,
        why: None,
        })
        .collect();
    let callees = vec![CallerCallee {
        name: "_sdsnewlen".into(),
        file: "sds.c".into(),
        line: 98,
        hop: 1,
        lang: Some("c".into()),
        cluster: Some("core:c".into()),
        relation: None,
    cite: None,
        why: None,
    }];
    // Tight fuse so packer must omit some callers (char budget still large).
    let pack = crate::server::trace_pack::pack_trace_neighbors_focus(
        callers,
        callees,
        &g,
        crate::server::trace_pack::TracePackConfig {
            char_budget: 12_000,
            hard_ceiling: 12,
            callees_first: true,
            ..crate::server::trace_pack::TracePackConfig::default()
        },
        &[],
        &[],
        &[],
    )
    .0;
    assert!(
        !pack.callees.is_empty(),
        "callee must survive pack when callers dominate; callers={} callees={}",
        pack.callers.len(),
        pack.callees.len()
    );
    assert_eq!(pack.callees[0].name, "_sdsnewlen");
    assert!(pack.callers_omitted() > 0);
    assert_eq!(pack.truncation_reason, Some("hard_ceiling"));
}

#[test]
fn dense_empty_callers_c_public_api_mentions_export() {
    let st = StructuredReport {
        state: StateInfo {
            edge_build: "100% | Complete".into(),
            jit: "None".into(),
            confidence: Some("edges_full".into()),
            percent: Some(100),
        },
        error: None,
        target: Some(TargetInfo {
            name: "glfwInit".into(),
            file: "src/init.c".into(),
            line: 10,
            definition: Some("int glfwInit(void) {\n  return 1;\n}".into()),
            lang: Some("cpp".into()),
            cluster: Some("core:c".into()),
        }),
        callers: vec![],
        callees: vec![],
        caller_path: vec![],
        peer_callers: vec![],
        bridge_callers: vec![],
        bridge_callees: vec![],
        blast_domain: Some("call".into()),
        seed_kind: Some("function_definition".into()),
        receipt: None,
        next_action: None,
        telemetry: serde_json::json!({}),
        suggested_scopes: vec![],
        skeleton: None,
        hubs: None,
        module_resolved_from: None,
        module_interior_candidates: None,
        locations: Some(vec![
            crate::server::dto::SymbolLocation {
                name: "glfwInit".into(),
                file: "src/init.c".into(),
                line: 10,
                end_line: Some(20),
                kind: "function_definition".into(),
                preferred: true,
                lang: Some("cpp".into()),
                cluster: Some("core:c".into()),
            },
            crate::server::dto::SymbolLocation {
                name: "glfwInit".into(),
                file: "include/GLFW/glfw3.h".into(),
                line: 42,
                end_line: None,
                kind: "function_declaration".into(),
                preferred: false,
                lang: Some("cpp".into()),
                cluster: Some("core:c".into()),
            },
        ]),
        clusters: None,
        bridges: None,
        active_cluster: Some("core:c".into()),
    };
    let dense =
        orchestrate_content_summary(Some(&st), None, ContentDetail::Dense);
    assert!(
        dense.contains("public API") || dense.contains("export"),
        "expected public-API fallback, got:\n{dense}"
    );
    assert!(
        dense.contains("glfw3.h") || dense.contains("header"),
        "expected header hint, got:\n{dense}"
    );
    assert!(
        dense.contains("not dead code") || dense.contains("not proof of dead"),
        "0 CALL must not sound like dead code:\n{dense}"
    );
    // Zero real callees — header must not appear as a fake call target.
    assert!(
        dense.contains("callees: (none in scope / graph)")
            || !dense.contains("callees ("),
        "implements edge must not invent callees:\n{dense}"
    );
    // Rust 0-CALL: general honesty (callback/framework), not silent empty line.
    let mut rust = sample_trace();
    rust.callers.clear();
    rust.telemetry = serde_json::json!({
        "seed_in_degree": 0,
        "seed_out_degree": 1,
        "edges_complete": true,
    });
    let rust_dense =
        orchestrate_content_summary(Some(&rust), None, ContentDetail::Dense);
    assert!(
        rust_dense.contains("not proof of dead code")
            || rust_dense.contains("not dead code")
            || rust_dense.contains("callback"),
        "rust 0 CALL must warn not dead code: {rust_dense}"
    );
}
