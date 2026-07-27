//! Integration tests for the code_graph parser and basic graph operations.
//!
//! These tests use the small example files in `examples/test_data/`.

use code_graph::{parse_file, CodeGraph, ParseError};
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc, RwLock};

/// Helper to locate the test data directory reliably.
fn test_data_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("examples/test_data")
}

fn read_test_file(name: &str) -> (PathBuf, String) {
    let path = test_data_dir().join(name);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("Test data file not found: {}", path.display()));
    (path, source)
}

#[test]
fn test_parse_rust_example_file() {
    let (path, source) = read_test_file("rust_example.rs");

    let parsed = parse_file(&path, &source).expect("Rust parsing should succeed");
    let blocks = parsed.blocks;

    assert_eq!(
        blocks.len(),
        1,
        "rust_example.rs should contain exactly one top-level item"
    );

    let block = &blocks[0];
    assert_eq!(block.name, "hello_rust");
    assert_eq!(block.kind, "function_item");
    assert_eq!(block.lang, "rust");
    assert_eq!(block.start_line, 1);
    assert_eq!(block.end_line, 3);
    assert!(block.source.contains("hello_rust"));
    assert!(block.source.contains("Hello from Rust"));
}

#[test]
fn test_parse_python_example_file() {
    let (path, source) = read_test_file("python_example.py");

    let parsed = parse_file(&path, &source).expect("Python parsing should succeed");
    let blocks = parsed.blocks;

    assert_eq!(
        blocks.len(),
        1,
        "python_example.py should contain exactly one top-level function"
    );

    let block = &blocks[0];
    assert_eq!(block.name, "hello_python");
    assert_eq!(block.kind, "function_definition");
    assert_eq!(block.lang, "python");
    assert_eq!(block.start_line, 1);
    assert_eq!(block.end_line, 2);
    assert!(block.source.contains("hello_python"));
    assert!(block.source.contains("Hello from Python"));
}

#[test]
fn test_parse_file_unknown_extension_returns_error() {
    let path = PathBuf::from("some_file.txt");
    let source = "hello world";

    let result = parse_file(&path, source);

    assert!(matches!(result, Err(ParseError::UnknownLanguage(_))));
    if let Err(ParseError::UnknownLanguage(ext)) = result {
        assert_eq!(ext, "txt");
    }
}

#[test]
fn test_code_graph_basic_operations() {
    let mut graph = CodeGraph::new();

    // Manually construct a minimal BlockInfo for testing graph structure
    // (we avoid depending on internal constructors)
    let block1 = code_graph::BlockInfo {
        id: code_graph::Id::new("file.rs", "function_item", "abc12345"),
        name: "foo".to_string(),
        file: PathBuf::from("file.rs"),
        kind: "function_item".to_string(),
        lang: "rust".to_string(),
        start_line: 10,
        end_line: 15,
        start_byte: 100,
        end_byte: 200,
        parent_id: None,
        children: vec![],
        content_hash: "abc12345".to_string(),
        sig_hash: "sig1".to_string(),
        git_blame_recency: None,
        git_author: None,
        has_cycle: false,
        is_macro_expanded: false,
        source: "fn foo() {}".to_string(),
        score: 0.0,
        usages: vec![],
        external_crates: Default::default(),
        is_highly_connected: false,
    };

    let block2 = code_graph::BlockInfo {
        id: code_graph::Id::new("file.rs", "function_item", "def67890"),
        name: "bar".to_string(),
        file: PathBuf::from("file.rs"),
        kind: "function_item".to_string(),
        lang: "rust".to_string(),
        start_line: 17,
        end_line: 20,
        start_byte: 220,
        end_byte: 280,
        parent_id: None,
        children: vec![],
        content_hash: "def67890".to_string(),
        sig_hash: "sig2".to_string(),
        git_blame_recency: None,
        git_author: None,
        has_cycle: false,
        is_macro_expanded: false,
        source: "fn bar() { foo(); }".to_string(),
        score: 0.0,
        usages: vec![],
        external_crates: Default::default(),
        is_highly_connected: false,
    };

    let id1 = block1.id.clone();
    let id2 = block2.id.clone();

    graph.add_block(block1);
    graph.add_block(block2);
    graph.add_edge(id1.clone(), id2.clone()); // foo -> bar (calls)

    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.children(&id1).len(), 1);
    assert_eq!(graph.callers(&id2).len(), 1);
    assert!(graph.get_block(id1).is_some());
}

#[test]
fn test_parse_rust_and_python_together() {
    let mut graph = CodeGraph::new();

    // Parse Rust file
    let (rust_path, rust_src) = read_test_file("rust_example.rs");
    let rust_parsed = parse_file(&rust_path, &rust_src).expect("rust parse");
    for b in rust_parsed.blocks {
        graph.add_block(b);
    }

    // Parse Python file
    let (py_path, py_src) = read_test_file("python_example.py");
    let py_parsed = parse_file(&py_path, &py_src).expect("python parse");
    for b in py_parsed.blocks {
        graph.add_block(b);
    }

    // We should now have blocks from both languages in the same graph
    let rust_count = graph.nodes.values().filter(|b| b.lang == "rust").count();
    let python_count = graph.nodes.values().filter(|b| b.lang == "python").count();

    assert_eq!(rust_count, 1);
    assert_eq!(python_count, 1);
    assert_eq!(graph.nodes.len(), 2);
}

// =============================================================================
// Scanner + skip pattern tests
// =============================================================================

use code_graph::snooper::scanner::{get_skip_patterns, scan_workspace, should_scan_path};

#[test]
fn test_get_skip_patterns_includes_defaults() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap(); // go up to workspace root

    // After refactor, pass the config defaults (now owned by ButlerSettings in cli).
    // For this test we simulate the full defaults list (must match cli/src/config.rs default).
    let config_skips: Vec<String> = vec![
        ".butler/".into(),
        ".git/".into(),
        "target/".into(),
        "node_modules/".into(),
        "__pycache__/".into(),
        ".cache/".into(),
        "build/".into(),
        "dist/".into(),
        "out/".into(),
        "vendor/".into(),
        ".idea/".into(),
        ".pytest_cache/".into(),
        ".ruff_cache/".into(),
        ".mypy_cache/".into(),
        ".cargo/".into(),
    ];
    let patterns = get_skip_patterns(root, &config_skips);

    assert!(patterns.iter().any(|p| p.contains("target/")));
    assert!(patterns.iter().any(|p| p.contains(".git/")));
    assert!(patterns.iter().any(|p| p.contains("node_modules/")));
}

#[test]
fn test_should_scan_path_respects_extension_and_ignore() {
    let skip = vec!["target/".to_string(), ".git/".to_string()];

    assert!(should_scan_path(&PathBuf::from("src/main.rs"), &skip));
    assert!(should_scan_path(&PathBuf::from("foo/bar.py"), &skip));
    assert!(!should_scan_path(
        &PathBuf::from("target/debug/foo.rs"),
        &skip
    ));
    assert!(!should_scan_path(&PathBuf::from("src/lib.txt"), &skip));
}

#[test]
fn test_scan_workspace_on_test_data() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_data = manifest.join("examples/test_data");

    // Pass empty config skips for the small test_data dir ( .butlerignore at root will still be considered if present at test_data, but typically we rely on passed list + ignore).
    let config_skips: Vec<String> = vec![];
    let graph = scan_workspace(&test_data, None, &config_skips);

    // We have two small files → at least 2 blocks (one Rust fn + one Python fn)
    assert!(
        graph.nodes.len() >= 2,
        "scan_workspace on test_data should find at least the Rust and Python example functions"
    );

    let has_rust = graph.nodes.values().any(|b| b.lang == "rust");
    let has_python = graph.nodes.values().any(|b| b.lang == "python");

    assert!(has_rust, "Should have parsed at least one Rust block");
    assert!(has_python, "Should have parsed at least one Python block");

    // Phase 3: edge building is now lazy (removed from initial scan in do_scan_workspace)
    assert!(
        graph.edges.is_empty(),
        "After scan_workspace (Phase 3 skeleton-first), call/usage edges must be empty until ensure_call_graph is called"
    );

    // Phase 4: Query-driven deepening simulation
    // In a real Surgical request (target_file + target_line present), run_context_logic
    // now proactively calls ensure_call_graph before selection/composition.
    // Here we simulate the same pattern the server uses for Surgical mode.
    let mut g: CodeGraph = graph.clone();
    // Simulate the server path for Surgical/Trace requests (or legacy full ensure).
    // Passing None triggers the full (or background-completed) edge build.
    g.ensure_call_graph(&test_data, &config_skips, None);
    assert_eq!(
        g.nodes.len(),
        graph.nodes.len(),
        "ensure_call_graph must preserve the node set"
    );
    // (In a real project with fn calls, g.edges would now be populated.)
}

#[test]
fn test_ensure_call_graph_jit_with_target_files() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_data = manifest.join("examples/test_data");
    let config_skips: Vec<String> = vec![];

    let graph = scan_workspace(&test_data, None, &config_skips);

    // Should start as pure skeleton
    assert!(
        graph.edges.is_empty(),
        "initial scan must be skeleton (no edges)"
    );
    assert!(
        graph.files_with_edges.is_empty(),
        "initial scan must have no files marked with edges"
    );
    assert!(!graph.background_edge_build_complete);

    // Pick only the Rust file for surgical/JIT edge build
    let rust_file = test_data.join("rust_example.rs");
    let target_files = vec![rust_file.clone()];

    let mut g = graph.clone();
    g.ensure_call_graph(&test_data, &config_skips, Some(&target_files));

    // The targeted file should now be marked
    assert!(
        g.files_with_edges.contains(&rust_file),
        "JIT target file should be recorded in files_with_edges"
    );

    // Python file should not have been processed in this surgical call
    let py_file = test_data.join("python_example.py");
    assert!(
        !g.files_with_edges.contains(&py_file),
        "non-targeted file must not be marked after surgical ensure_call_graph"
    );

    // We don't assert on edges here because the tiny example may have zero call edges,
    // but the important thing is that the machinery ran and marked only the requested files.
    // (Real projects with calls will populate edges for the targeted subset.)
}

#[test]
fn test_run_background_full_edge_build() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_data = manifest.join("examples/test_data");
    let config_skips: Vec<String> = vec![];

    let graph = scan_workspace(&test_data, None, &config_skips);
    let graph_rw = Arc::new(RwLock::new(graph));
    let cancel = Arc::new(AtomicBool::new(false));

    // Run the background builder (it should be fast on tiny test data)
    code_graph::snooper::run_background_full_edge_build(
        Arc::clone(&graph_rw),
        cancel,
        test_data.clone(),
        config_skips,
        Some(code_graph::snooper::BgBuildProgress::new(10)),
        0,
    );

    let final_graph = graph_rw.read().unwrap();
    // After full background build we expect the complete flag
    assert!(
        final_graph.background_edge_build_complete,
        "background build must set complete flag"
    );
    // All source files that were in the skeleton should now be marked
    assert!(
        !final_graph.files_with_edges.is_empty(),
        "background build should have marked files"
    );
}

#[test]
fn test_background_edge_build_cancellation() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_data = manifest.join("examples/test_data");
    let config_skips: Vec<String> = vec![];

    let graph = scan_workspace(&test_data, None, &config_skips);
    let graph_rw = Arc::new(RwLock::new(graph));
    let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled

    code_graph::snooper::run_background_full_edge_build(
        Arc::clone(&graph_rw),
        cancel,
        test_data,
        config_skips,
        Some(code_graph::snooper::BgBuildProgress::new(10)),
        0,
    );

    let g = graph_rw.read().unwrap();
    assert!(
        g.files_with_edges.is_empty(),
        "cancelled background build must not mark any files"
    );
    assert!(
        !g.background_edge_build_complete,
        "cancelled build must not set complete flag"
    );
    assert_eq!(
        g.background_edge_build_state,
        code_graph::snooper::BackgroundEdgeBuildState::Cancelled,
        "cancelled build must set Cancelled state (Sprint 5 zombie fix)"
    );
    assert!(
        !g.background_edge_build_active,
        "cancelled build must clear active flag"
    );
    assert!(
        g.needs_background_edge_resuscitation(),
        "cancelled graph should be eligible for resuscitation"
    );
}
