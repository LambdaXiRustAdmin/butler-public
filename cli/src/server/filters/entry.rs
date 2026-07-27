//! Entry landmarks, structural multipliers, type-trace helpers.
use super::noise::application_path_priority;
use code_graph::BlockInfo;
use std::path::Path;

/// Filenames that are usually product entry points (CLI drivers, crate roots).
const ENTRY_FILE_BASENAMES: &[&str] = &[
    "main.rs",
    "main.py",
    "main.go",
    "main.c",
    "main.cpp",
    "main.ts",
    "main.js",
    "mod.rs",
    "lib.rs",
    "__main__.py",
    "app.py",
    "server.py",
    "cli.py",
    "index.ts",
    "index.js",
    "emcc.py",
    "em++.py",
    "emscripten.py",
    "link.py",
    "compile.py",
    "building.py",
];

/// Function / symbol names that often mark entry or pipeline phases.
const ENTRY_SYMBOL_NAMES: &[&str] = &[
    "main",
    "run",
    "start",
    "cli",
    "app",
    "serve",
    "emscript",
    "compile_javascript",
    "phase_setup",
    "phase_compile_inputs",
    "phase_linker_setup",
];

/// True when this block is a likely **entry / CLI / pipeline** landmark (not degree-based).
pub fn is_entry_landmark(block: &BlockInfo, project_root: &Path) -> bool {
    let file_str = normalize_path_for_entry(&block.file);
    let base = Path::new(&file_str)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let name_l = block.name.to_ascii_lowercase();

    if ENTRY_FILE_BASENAMES
        .iter()
        .any(|b| base.eq_ignore_ascii_case(b))
    {
        // Prefer real defs in those files over tiny helpers.
        if block.name == "main"
            || ENTRY_SYMBOL_NAMES.iter().any(|s| name_l == *s)
            || block.kind.contains("function")
            || block.kind.contains("method")
            || name_l.starts_with("phase_")
        {
            return true;
        }
        // Whole-module / class roots in entry files.
        if block.kind.contains("module") || block.kind.contains("class") {
            return true;
        }
    }

    if ENTRY_SYMBOL_NAMES.iter().any(|s| name_l == *s) {
        // Avoid test / third_party mains when possible.
        if is_testish_path(&file_str) {
            return false;
        }
        return true;
    }

    if name_l.starts_with("phase_") && file_str.contains("/tools/") {
        return true;
    }

    // Script entry: if __name__ == "__main__" lives near this def (cheap substring).
    if !block.source.is_empty()
        && (block.source.contains("if __name__") || block.source.contains("__main__"))
        && (name_l == "main" || block.kind.contains("function"))
    {
        return true;
    }

    // Repo-root CLI scripts (depth ≤ 1 under project root).
    if is_repo_root_script(&file_str, project_root) && block.name == "main" {
        return true;
    }

    false
}

/// Multiplier for Arch hub ranking — entry landmarks over pure degree hubs.
pub fn entry_point_multiplier(block: &BlockInfo, project_root: &Path) -> f64 {
    if !is_entry_landmark(block, project_root) {
        return 1.0;
    }
    let file_str = normalize_path_for_entry(&block.file);
    let base = Path::new(&file_str)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut m = 8.0;
    if base.eq_ignore_ascii_case("emcc.py")
        || base.eq_ignore_ascii_case("em++.py")
        || base.eq_ignore_ascii_case("main.rs")
        || base.eq_ignore_ascii_case("main.py")
    {
        m = 24.0;
    } else if ENTRY_FILE_BASENAMES
        .iter()
        .any(|b| base.eq_ignore_ascii_case(b))
    {
        m = 16.0;
    }
    if block.name == "main" {
        m *= 1.5;
    }
    if block.name.starts_with("phase_") {
        m *= 1.25;
    }
    m
}

fn normalize_path_for_entry(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn is_testish_path(file_str: &str) -> bool {
    let s = file_str.to_ascii_lowercase().replace('\\', "/");
    s.contains("/test/")
        || s.contains("/tests/")
        || s.contains("/testing/")
        || s.contains("/benchmarks/")
        || s.contains("/benchmark/")
        || s.contains("/benches/")
        || s.contains("/bench/")
        || s.contains("_test.")
        || s.contains("/third_party/")
        || s.contains("/third-party/")
        || s.contains("/vendor/")
}

fn is_repo_root_script(file_str: &str, project_root: &Path) -> bool {
    let root = normalize_path_for_entry(project_root);
    let f = file_str.trim_start_matches('/');
    let r = root.trim_start_matches('/');
    let rel = f.strip_prefix(r).unwrap_or(f).trim_start_matches('/');
    // one component: emcc.py at root
    !rel.is_empty() && !rel.contains('/')
}

/// Quick primitive/boilerplate detector for hub filtering.
pub fn is_hub_primitive(
    name: &str,
    in_degree: usize,
    out_degree: usize,
    total_nodes: usize,
) -> bool {
    if name.len() <= 2 {
        return true;
    }

    let n = name.to_lowercase();

    const UNIVERSAL_PRIMITIVES: &[&str] = &[
        "string", "f32", "f64", "i32", "u32", "usize", "bool", "err", "error", "result", "option",
        "new", "default", "from", "into", "clone", "drop",
    ];

    if UNIVERSAL_PRIMITIVES.contains(&n.as_str()) {
        return true;
    }

    let threshold = std::cmp::max(10, total_nodes / 100);
    if in_degree > threshold && out_degree <= 2 {
        return true;
    }

    false
}

/// Boost real architecture types/traits for hub and trace ranking.
pub fn is_architectural_kind(kind: &str) -> bool {
    let k = kind.to_lowercase();
    k.contains("struct")
        || k.contains("class")
        || k.contains("trait")
        || k.contains("interface")
        || k.contains("enum")
        || k.contains("type_item")
        || k.contains("type_alias")
        || k.contains("impl")
        || k.contains("module")
}

/// True when trace should use usage/reference edges instead of call-only BFS.
pub fn is_type_trace_target(kind: &str) -> bool {
    let k = kind.to_lowercase();
    k.contains("struct_item")
        || k.contains("struct_specifier")
        || k.contains("class_item")
        || k.contains("class_specifier") // tree-sitter-cpp
        || k.contains("class_definition")
        || k.contains("class_declaration")
        || k.contains("type_alias")
        || k.contains("type_item")
        || k.contains("type_spec") // Go type_spec (struct/interface)
        || k.contains("enum_item")
        || k.contains("enum_specifier")
        || k.contains("interface_declaration")
        || k.contains("interface_specifier")
}

/// Product blast domain for Trace (Track P.5).
///
/// - `call` — function/method; CALL edges are authoritative for signature changes.
/// - `type_neighborhood` — type/struct seed; CALL Trace is **not** full ABI/layout blast.
pub fn blast_domain_for_seed_kind(kind: &str) -> &'static str {
    if is_type_trace_target(kind) {
        "type_neighborhood"
    } else {
        "call"
    }
}

/// Structural multiplier for ranking (used in sort_by).
pub fn structural_multiplier(
    kind: &str,
    in_degree: usize,
    out_degree: usize,
    total_nodes: usize,
) -> f64 {
    let k = kind.to_lowercase();
    let threshold = std::cmp::max(20, total_nodes / 50);

    if k.contains("primitive") || k.contains("builtin") || k.contains("macro") {
        0.0
    } else if k.contains("class")
        || k.contains("struct")
        || k.contains("trait")
        || k.contains("interface")
        || k.contains("impl")
    {
        1.5
    } else if k.contains("fn") || k.contains("method") || k.contains("function") {
        1.0
    } else if in_degree > threshold && out_degree <= 2 {
        0.1
    } else {
        0.5
    }
}


/// True for a same-named type definition in a different file (homonym collision).
pub fn is_homonym_type_def(candidate: &BlockInfo, target: &BlockInfo) -> bool {
    candidate.id != target.id
        && candidate.name == target.name
        && is_type_trace_target(&candidate.kind)
}

/// Skip peripheral reference sites when the resolved target lives in primary code.
pub fn is_peripheral_relative_to_target(candidate: &BlockInfo, target: &BlockInfo) -> bool {
    let cp = application_path_priority(&candidate.file.to_string_lossy());
    let tp = application_path_priority(&target.file.to_string_lossy());
    tp >= 80 && cp <= 20
}
