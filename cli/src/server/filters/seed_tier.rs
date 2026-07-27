//! Seed role tiers + candidate set filters (relative retains).
use super::entry::is_type_trace_target;
use super::noise::is_noise_test_filename;
use code_graph::BlockInfo;

/// Named tier ladder for [`seed_role_tier`] / relative filters (do not reshuffle without keepers).
pub const TIER_NEVER: i32 = 0;
pub const TIER_CALL: i32 = 10;
pub const TIER_MODULE: i32 = 30;
pub const TIER_TYPE_ALIAS: i32 = 45;
pub const TIER_BINDING: i32 = 50;
pub const TIER_IMPL: i32 = 70;
pub const TIER_FUNCTION: i32 = 90;
pub const TIER_TYPE: i32 = 100;

/// Orchestrate seed role tier (higher = better). Polyglot: kinds from Rust/Py/TS/Go/C++.
///
/// | Const | Value | Role |
/// |-------|------:|------|
/// | [`TIER_TYPE`] | 100 | Type definitions (struct/class/enum/interface/trait/…) |
/// | [`TIER_FUNCTION`] | 90 | Function / method definitions |
/// | [`TIER_IMPL`] | 70 | Impl blocks / secondary type carriers |
/// | [`TIER_BINDING`] | 50 | Value bindings (const/static), named arrows |
/// | [`TIER_TYPE_ALIAS`] | 45 | Thin aliases / using |
/// | [`TIER_MODULE`] | 30 | Module shells (`mod foo;`) |
/// | [`TIER_CALL`] | 10 | Calls, uses, weak declarators |
/// | [`TIER_NEVER`] | 0 | Control-flow / statement noise — never prefer as seed |
///
/// Match order matters (`.contains` chains): first hit wins. Do not reorder without dual-stack keepers.
pub fn seed_role_tier(kind: &str) -> i32 {
    let k = kind.to_lowercase();

    // --- Never (control-flow / statement noise) ---
    if k.contains("if_expression")
        || k.contains("if_statement")
        || k.contains("if_let")
        || k.contains("for_expression")
        || k.contains("for_statement")
        || k.contains("while_expression")
        || k.contains("while_statement")
        || k.contains("loop_expression")
        || k.contains("match_expression")
        || k.contains("match_arm")
        || k.contains("return_expression")
        || k.contains("return_statement")
        || k.contains("let_declaration")
        || k.contains("let_statement")
        || k.contains("assignment_expression")
        || k.contains("assignment_statement")
        || k.contains("range_clause")
        || k.contains("expression_statement")
    {
        return TIER_NEVER;
    }

    // --- Calls / imports (last resort) ---
    if k.contains("call_expression")
        || k == "call"
        || k.contains("use_declaration")
        || k.contains("import_statement")
        || k.contains("import_from")
    {
        return TIER_CALL;
    }

    // --- Modules (forwarding shells — lose to real defs) ---
    if k.contains("mod_item") || k == "module" || k.contains("namespace") {
        return TIER_MODULE;
    }

    // --- Types (best for type-shaped queries) ---
    // Prefer real class/struct/enum bodies over thin aliases (forward/using demoted later).
    if k.contains("type_alias") || k.contains("using_declaration") {
        return TIER_TYPE_ALIAS;
    }
    if is_type_trace_target(kind)
        || k.contains("trait_item")
        || k.contains("union_item")
        || k.contains("enum_item")
        || k.contains("type_spec") // Go struct/interface
    {
        return TIER_TYPE;
    }

    // --- Functions / methods ---
    if k.contains("function_item")
        || k.contains("function_definition")
        || k.contains("function_declaration")
        || k.contains("async_function")
        || k.contains("method_definition")
        || k.contains("method_declaration")
        || k.contains("method_item")
    {
        return TIER_FUNCTION;
    }

    // --- Impl / type-adjacent ---
    if k.contains("impl_item") || k.contains("impl_definition") {
        return TIER_IMPL;
    }

    // --- Bindings / arrows ---
    if k.contains("const_item")
        || k.contains("static_item")
        || k.contains("arrow_function")
        || k.contains("lexical_declaration")
        || k.contains("variable_declarator")
        || k.contains("short_var_declaration")
    {
        return TIER_BINDING;
    }

    // Unknown / misc structural
    if k.contains("unknown") {
        return 5;
    }

    20
}

/// True if this block looks like unit-test *code* (file or name), not `test_repos` folders.
pub fn is_testish_seed_block(b: &BlockInfo) -> bool {
    let path = b.file.to_string_lossy().replace('\\', "/").to_lowercase();
    let file_name = b
        .file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    if is_noise_test_filename(&file_name) {
        return true;
    }

    // Exact noise-dir segments (universal monorepo conventions — not product spines).
    if path.split('/').any(|seg| {
        matches!(
            seg,
            "tests"
                | "test"
                | "testing"
                | "testutil"
                | "testdata"
                | "__tests__"
                | "benchmarks" // A′.10: microbench trees dominate CALL Trace (rich Console)
                | "benchmark"
                | "benches"
                | "bench"
                | "generated"
                | "gen"
                | "third_party"
                | "third-party"
                | "vendor"
                | "vendored"
                | "node_modules"
                | "site-packages"
                | "__pycache__"
                | "virtual"
                | "fixtures"
                | "mocks"
                | "testdoubles"
        )
    }) {
        return true;
    }

    let n = b.name.to_lowercase();
    if n.starts_with("test_") || n.ends_with("_test") || n.ends_with("_tests") {
        return true;
    }

    // Thin call-shaped body often from test call sites: `load_weights(".")`
    let src = b.source.trim();
    if !src.is_empty()
        && src.len() < 120
        && !src.contains('{')
        && src.contains('(')
        && (src.starts_with(&b.name) || src.contains(&format!("{}(", b.name)))
        && seed_role_tier(&b.kind) <= 10
    {
        return true;
    }

    false
}

/// C/C++ (and similar) forward decl / thin shell — not the definition a human means.
///
/// e.g. `struct TensorImpl;` in MemoryOverlap.h vs real class body in TensorImpl.h.
pub fn is_forward_or_shell_seed(b: &BlockInfo) -> bool {
    let k = b.kind.to_ascii_lowercase();
    let src = b.source.trim();
    if src.is_empty() {
        // Slim warehouse: no body text — treat tiny span as shell-ish.
        return b.end_line.saturating_sub(b.start_line) <= 1
            && (k.contains("struct") || k.contains("class") || k.contains("type"));
    }
    let lower = src.to_ascii_lowercase();
    // `struct Foo;` / `class Foo;` / `enum Foo;` — no body braces.
    if (k.contains("struct") || k.contains("class") || k.contains("enum") || k.contains("type"))
        && !src.contains('{')
        && src.ends_with(';')
        && src.len() < 200
    {
        return true;
    }
    // typedef / using aliases without bodies
    if (k.contains("type_alias") || k.contains("type_definition") || k.contains("type_item"))
        && !src.contains('{')
        && (lower.starts_with("using ") || lower.starts_with("typedef ") || src.ends_with(';'))
        && src.len() < 240
    {
        return true;
    }
    false
}

/// Filename stem matches symbol (TensorImpl.h / TensorImpl.cpp → TensorImpl).
/// Codebases almost always name the primary file after the type.
pub fn filename_matches_symbol(b: &BlockInfo) -> i32 {
    let stem = b
        .file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = b.name.to_ascii_lowercase();
    if stem.is_empty() || name.is_empty() {
        return 0;
    }
    if stem == name {
        // Stronger than path-depth noise: primary Type.h / Type.cpp door.
        return 120;
    }
    // Private/public twin files: `_tensor.py` for class Tensor (Python packages).
    // Same weight as exact stem so path-context (impl tree vs product spine) can decide.
    if stem == format!("_{name}") || stem == format!("{name}_") {
        return 120;
    }
    // Cousin files (`FooBar` / `BarFoo` when query is `Foo`) must not beat exact `Foo.h`.
    // Only exact stem match gets the big bonus; longer stems that embed the name are demoted.
    if stem.len() > name.len() && (stem.ends_with(&name) || stem.starts_with(&name)) {
        return -45;
    }
    0
}

/// Real type hub (class/struct/enum/…) with a body — not a forward shell.
pub fn is_type_seed_hub(b: &BlockInfo) -> bool {
    seed_role_tier(&b.kind) >= 100 && !is_forward_or_shell_seed(b)
}

/// Emulated constructor/destructor knowledge (tree-sitter-cpp has no `constructor` kind).
///
/// **Problem class:** same identifier names the type hub and its ctor/dtor; CALL edges
/// hang off the type. Prefer type when both exist. Signals (any):
/// - name is `constructor` / `__init__` (TS / Python)
/// - destructor name (`~Foo`)
/// - function whose name equals the file stem (`Type` in `Type.h`)
/// - out-of-line C++ `Type::Type(` body text
pub fn is_likely_constructor_or_destructor(b: &BlockInfo) -> bool {
    let k = b.kind.to_ascii_lowercase();
    let is_fn = k.contains("function_definition")
        || k.contains("function_declaration")
        || k.contains("function_item")
        || k.contains("method_definition")
        || k.contains("method_declaration")
        || k.contains("method_item");
    if !is_fn {
        return false;
    }
    let name = b.name.trim();
    if name.is_empty() {
        return false;
    }
    let name_l = name.to_ascii_lowercase();
    if name_l == "constructor" || name_l == "__init__" || name_l == "__del__" {
        return true;
    }
    // tree-sitter-cpp: destructor_name → often stored with leading `~`
    if name.starts_with('~') {
        return true;
    }
    let stem = b
        .file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Out-of-line: `TensorImpl::TensorImpl(` / `TensorImpl::TensorImpl{`
    // (Do **not** treat every `Type` function in `Type.h` as a ctor — that demoted the
    // basename-correct file vs a free function of the same name in another header.)
    let src = b.source.trim();
    if !src.is_empty() {
        let compact: String = src.chars().filter(|c| !c.is_whitespace()).take(400).collect();
        let needle_paren = format!("{name}::{name}(");
        let needle_brace = format!("{name}::{name}{{");
        if compact.contains(&needle_paren) || compact.contains(&needle_brace) {
            return true;
        }
    }
    // Same-file door only when stem matches **and** body looks like a ctor (empty/init).
    if !stem.is_empty() && stem == name_l {
        if src.is_empty() {
            // Slim warehouse: tiny span free fn in Type.h is ctor-ish; multi-line is type door.
            return b.end_line.saturating_sub(b.start_line) <= 3;
        }
        // `Type()` / `Type(args) {` without `::` — in-class or free ctor form.
        let compact: String = src.chars().filter(|c| !c.is_whitespace()).take(200).collect();
        if compact.contains(&format!("{name}(")) && !compact.contains("::") {
            return true;
        }
    }
    false
}

/// Relative silent-drop filter for seed candidates.
///
/// **Not a per-block absolute filter:** each stage keeps or drops based on what else
/// is still in the set (e.g. if any type/fn exists, modules/calls leave). The same
/// block may survive alone but drop when a better peer is present — intentional.
///
/// Stages (in order): never-tier out → raise floor to binding/type/fn → prefer non-test
/// → drop forward shells if a body exists → drop ctor doors if a type hub exists.
pub fn filter_seed_candidates<'a>(candidates: Vec<&'a BlockInfo>) -> Vec<&'a BlockInfo> {
    let mut v: Vec<&BlockInfo> = candidates
        .into_iter()
        .filter(|b| !b.name.is_empty() && seed_role_tier(&b.kind) > TIER_NEVER)
        .collect();
    if v.is_empty() {
        return v;
    }

    let max_tier = v.iter().map(|b| seed_role_tier(&b.kind)).max().unwrap_or(0);

    // Definitions present (types/fns/impl/bindings) → drop modules + calls
    if max_tier >= TIER_BINDING {
        v.retain(|b| seed_role_tier(&b.kind) >= TIER_BINDING);
    }
    // Types or functions present → drop pure bindings; type(100)+fn(90)+impl(70) compete.
    let max2 = v.iter().map(|b| seed_role_tier(&b.kind)).max().unwrap_or(0);
    if max2 >= TIER_FUNCTION {
        v.retain(|b| seed_role_tier(&b.kind) >= TIER_FUNCTION);
    }

    // Prefer non-test when any non-test remains
    if v.iter().any(|b| !is_testish_seed_block(b)) {
        v.retain(|b| !is_testish_seed_block(b));
    }

    // Real body present → drop forward decls / thin shells (MemoryOverlap `struct Foo;`).
    if v.iter()
        .any(|b| !is_forward_or_shell_seed(b) && seed_role_tier(&b.kind) >= TIER_FUNCTION)
    {
        v.retain(|b| !is_forward_or_shell_seed(b));
    }

    // Type hub present → drop ctor/dtor doors (prefer class body over same-named ctor).
    if v.iter().any(|b| is_type_seed_hub(b)) {
        v.retain(|b| !is_likely_constructor_or_destructor(b));
    }

    v
}
