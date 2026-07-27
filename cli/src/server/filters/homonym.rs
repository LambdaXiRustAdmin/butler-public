//! Homonym ranking, qualification evidence, trace-noise names.
use super::noise::application_path_priority;
use super::seed_tier::{
    filter_seed_candidates, filename_matches_symbol, is_forward_or_shell_seed,
    is_likely_constructor_or_destructor, is_testish_seed_block, seed_role_tier,
};
use code_graph::BlockInfo;

fn is_impl_tree_path(f: &str) -> bool {
    f.contains("/csrc/")
        || f.contains("/aten/")
        || f.contains("/caffe2/")
        || f.contains("/tensorexpr/")
        || f.contains("/third_party/")
        || f.contains("/third-party/")
        || f.contains("/_deps/")
}

/// User-facing package spines (product API over compiler/runtime guts).
/// Does **not** global-prefer `.py` — pure C++ repos keep winning on their own trees.
fn is_public_product_spine(f: &str) -> bool {
    if is_impl_tree_path(f) {
        return false;
    }
    // PyTorch / dual-stack public packages
    if f.contains("/torch/nn/")
        || f.contains("/torch/distributed/")
        || f.contains("/torch/optim/")
        || f.contains("/torch/utils/")
        || f.contains("/torch/fx/")
        || f.contains("/torch/autograd/") && f.ends_with(".py")
    {
        return true;
    }
    // Generic product layouts (not nested under csrc/aten)
    if f.contains("/packages/")
        || f.contains("/pkg/")
        || (f.contains("/lib/") && !f.contains("/libshm/"))
        || f.contains("/apps/")
    {
        return true;
    }
    // Shallow package entry: `torch/tensor.py`, `src/foo.py` (not deep csrc)
    let depth = f.matches('/').count();
    if depth <= 3 && (f.ends_with(".py") || f.ends_with(".go") || f.ends_with(".ts")) {
        return true;
    }
    false
}

/// Path / package context for homonyms (gin.Default vs binding.Default).
/// Higher = more likely the “public API” a human means.
///
/// **Dominant terms (order of magnitude):** test −120, impl-tree −70(−30), binding/internal −40,
/// public spine +55, basename-ish via filename_matches elsewhere, depth −3/slash.
/// Tuned under dual-stack keepers — do not rebalance without evidence.
fn homonym_context_score(b: &BlockInfo) -> i32 {
    let f = b.file.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let mut s = 0i32;
    if is_testish_seed_block(b) {
        s -= 120;
    }
    // Nested binding / internal packages lose to package-root API files.
    if f.contains("/binding/")
        || f.contains("/bindings/")
        || f.contains("/internal/")
        || f.contains("/detail/")
        || f.contains("/private/")
    {
        s -= 40;
    }
    if f.contains("/examples/") || f.contains("/fixtures/") || f.contains("/bench") {
        s -= 50;
    }
    // Implementation trees lose to product spines on mega-homonyms (Module, Tensor).
    // Magnitude must beat include-path + large class body span later in the ladder.
    if is_impl_tree_path(&f) {
        s -= 70;
        // Nested DSL / expr IR is almost never the user-facing type of the same name.
        if f.contains("/tensorexpr/") {
            s -= 30;
        }
    }
    if is_public_product_spine(&f) {
        s += 55;
    }
    if f.contains("/src/") || f.contains("/include/") || f.contains("/lib/") || f.contains("/tools/")
    {
        s += 25;
    }
    // c10/ / core/ core types (pytorch) — prefer over random utility headers.
    // Still valid under demoted aten/csrc when no public twin (TensorImpl).
    if f.contains("/c10/") || f.contains("/core/") {
        s += 20;
    }
    // Shallower path ≈ package-level entry (gin/gin.go beats gin/binding/x.go).
    let depth = f.matches('/').count() as i32;
    s -= depth * 3;
    // Basename matches common entry files OR the symbol name (TensorImpl.h).
    let base = b
        .file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let name_l = b.name.to_ascii_lowercase();
    if matches!(
        base.as_str(),
        "main.rs"
            | "lib.rs"
            | "mod.rs"
            | "main.py"
            | "main.go"
            | "gin.go"
            | "__init__.py"
            | "index.ts"
            | "index.js"
            | "emcc.py"
            | "link.py"
    ) || base == format!("{}.go", name_l)
        || base == format!("{}.rs", name_l)
        || base == format!("{}.py", name_l)
        || base == format!("{}.h", name_l)
        || base == format!("{}.hpp", name_l)
        || base == format!("{}.hh", name_l)
        || base == format!("{}.cpp", name_l)
        || base == format!("{}.cc", name_l)
        || base == format!("{}.c", name_l)
    {
        s += 35;
    }
    s += filename_matches_symbol(b);
    // Forward decl shells lose hard.
    if is_forward_or_shell_seed(b) {
        s -= 50;
    }
    // Name Default/New/Open in a short package file is classic entry API.
    let nl = b.name.to_ascii_lowercase();
    if matches!(nl.as_str(), "default" | "new" | "open" | "main" | "init" | "run") {
        s += 15;
    }
    s
}

/// Hard-drop names for Trace neighbors (stdlib wrappers + ultra-generic ops).
/// Prefer omit over invent: accurate packs beat noisy completeness.
/// Domain methods (`register_module`, `load_state_dict`) stay.
///
/// # TODO(P2) — neighbor noise pack (pocket; do **not** block ship)
///
/// Telemetry on warm Complete monsters shows Trace packs already ~9–20 blocks after
/// seed+BFS caps — token economy is fine for now. Build P2 only if agent context
/// starts bleeding on macros / weak verbs after ★ is correct.
///
/// Blueprint (universal patterns, **no** product macros like `MOZ_ASSERT`):
/// - Macro-ish: ALL_CAPS + short / assert|check|impl name patterns, or kind=macro
/// - Stronger demotion of weak verbs (`get`/`set`/…) already partially here
/// - Cross-island demote vs seed path-island; keep alts list for re-pin
/// - Order: P0/P1/P3 seed routing first; P2 is brush-clear after coordinates are right
pub fn is_trace_noise_name(name: &str) -> bool {
    if name.len() < 3 {
        return true;
    }
    matches!(
        name,
        // Language std collections / wrappers
        "Box"
            | "HashMap"
            | "HashSet"
            | "Vec"
            | "Arc"
            | "Rc"
            | "Option"
            | "Result"
            | "String"
            // Ultra-generic ops that steal leviathan fan-out (not product APIs)
            | "next"
            | "iter"
            | "len"
            | "size"
            | "info"
            | "warning"
            | "debug"
            | "copy_"
            | "numel"
            | "move"
            | "find"
            | "hook"
            | "discard"
            | "typename"
            | "data"
            | "Node"
            | "items"
            | "keys"
            | "values"
            | "append"
            | "extend"
            | "begin"
            | "end"
            | "empty"
            | "clone"
            | "copy"
            | "print"
    )
}

/// Soft-rank penalty (not hard drop): common verbs that are sometimes real APIs.
/// Sinks below path-local neighbors before fan-out truncate.
pub fn trace_name_weak_penalty(name: &str) -> i32 {
    if is_trace_noise_name(name) {
        return 100;
    }
    if matches!(
        name,
        "load"
            | "save"
            | "open"
            | "close"
            | "read"
            | "write"
            | "update"
            | "clear"
            | "get"
            | "set"
            | "push"
            | "pop"
            | "insert"
            | "erase"
            | "type"
            | "format"
            | "log"
            | "error"
    ) {
        return 40;
    }
    0
}

/// True when file stem equals the block name (`nsCOMPtr.h` ↔ `nsCOMPtr`).
/// Universal C/Java/C# door convention — used as a **hard** seed constraint.
pub fn has_basename_symbol_match(b: &BlockInfo) -> bool {
    filename_matches_symbol(b) >= 120
}

/// Directed CALL/usage in-degree for a block (O(1) reverse lookup).
///
/// Pure graph gravity among **exact-name candidates only** — not global hub score.
/// Headers with many inbound edges beat leaf `.cpp` twins without product path spines.
///
/// # TODO(P3b) — structural gravity for interfaces (sparse CALL fan-in)
///
/// CALL-only reverse fails for abstract bases (`nsISupports`): nothing "calls" the
/// interface; gravity lives in inherits/implements/type-usage edges. When those
/// relation types exist in the warehouse, fold them into seed in-degree (still
/// universal topology — not product path spines). Until then P3 is CALL reverse only.
#[inline]
pub fn directed_in_degree(graph: &code_graph::CodeGraph, b: &BlockInfo) -> usize {
    graph.reverse.get(&b.id).map_or(0, |v| v.len())
}

/// When multiple blocks share `target_symbol`, prefer the declaration a human means.
///
/// Ladder (Query Planner — not GNN, not product path spines):
/// 1. **Hard:** if any candidate has basename≡symbol, rank only among those
/// 2. filter_seed_candidates (test/noise drop, type≫ctor, shells)
/// 3. basename → AST role → **directed in-degree** → C def → path context → body size
///
/// Global hub/`score` is ignored; in-degree is only among the filtered exact-name set.
///
/// Same as [`pick_best_homonym_with_in_degree`] using CALL/usage reverse edges from `graph`.
pub fn pick_best_homonym_on_graph<'a>(
    graph: &code_graph::CodeGraph,
    candidates: impl IntoIterator<Item = &'a BlockInfo>,
) -> Option<&'a BlockInfo> {
    pick_best_homonym_with_in_degree(candidates, |b| directed_in_degree(graph, b))
}

/// Path segment match for a namespace/type prefix token (portable — not product spines).
///
/// Uses exact path segments only (`/mozilla/` or `mozilla` as a component), never substring
/// of longer names. Last `::` component of multi-part parents is used (`a::b` → `b`).
pub fn path_has_ns_token(file: &std::path::Path, parent_or_token: &str) -> bool {
    let token = parent_or_token
        .rsplit("::")
        .next()
        .unwrap_or(parent_or_token)
        .trim();
    if token.is_empty() {
        return false;
    }
    let t = token.to_ascii_lowercase();
    file.to_string_lossy()
        .replace('\\', "/")
        .split('/')
        .any(|seg| !seg.is_empty() && seg.eq_ignore_ascii_case(&t))
}

/// True if any `parent` path segment (`mozilla` in `mozilla::detail`) appears as a path component.
pub fn path_has_any_parent_token(file: &std::path::Path, parent_name: &str) -> bool {
    parent_name
        .split("::")
        .filter(|t| !t.trim().is_empty())
        .any(|t| path_has_ns_token(file, t))
}

fn has_ancestor_named(graph: &code_graph::CodeGraph, mut cur: code_graph::Id, target: &str) -> bool {
    // Walk parent_id chain only (AST/container), O(depth).
    // Also match last `::` segment when target is multi-part (`mozilla::detail` vs node `detail`).
    let targets: Vec<&str> = std::iter::once(target)
        .chain(target.split("::").filter(|s| !s.is_empty()))
        .collect();
    for _ in 0..64 {
        let Some(b) = graph.get_block(cur.clone()) else {
            return false;
        };
        if targets.iter().any(|t| b.name == *t) {
            return true;
        }
        match &b.parent_id {
            Some(p) => cur = p.clone(),
            None => return false,
        }
    }
    false
}

/// Candidate absolute paths for seed peek (repo-relative + host ↔ container mounts).
///
/// Fallback chain: raw path → root.join(rel) → `BUTLER_HOST_MOUNT`/`BUTLER_CONTAINER_MOUNT`
/// rewrite both directions. Order is try-until-readable, not a priority rank.
fn seed_file_open_candidates(
    file: &std::path::Path,
    project_root: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let push = |out: &mut Vec<std::path::PathBuf>, p: std::path::PathBuf| {
        if !out.iter().any(|e| e == &p) {
            out.push(p);
        }
    };
    push(&mut out, file.to_path_buf());
    if let Some(root) = project_root {
        if file.is_relative() {
            push(&mut out, root.join(file));
        }
        // Also join when warehouse stored a host-absolute path under a different mount.
        let s = file.to_string_lossy().replace('\\', "/");
        if let Some(idx) = s.find("/test_repos/").or_else(|| s.find("/projects/")) {
            // last-resort: suffix under project root's parent chain is messy; prefer mount rewrite below
            let _ = idx;
        }
    }
    let s = file.to_string_lossy().replace('\\', "/");
    // Dual-mount rewrite: only when BUTLER_HOST_MOUNT / BUTLER_CONTAINER_MOUNT are set
    // (e.g. compose: host projects tree ↔ `/projects`). No personal home defaults.
    if let (Ok(host), Ok(cont)) = (
        std::env::var("BUTLER_HOST_MOUNT"),
        std::env::var("BUTLER_CONTAINER_MOUNT"),
    ) {
        let host = host.trim_end_matches('/').to_string();
        let cont = cont.trim_end_matches('/').to_string();
        if !host.is_empty() && !cont.is_empty() {
            if let Some(rest) = s.strip_prefix(&host) {
                push(&mut out, std::path::PathBuf::from(format!("{cont}{rest}")));
            }
            if let Some(rest) = s.strip_prefix(&cont) {
                push(&mut out, std::path::PathBuf::from(format!("{host}{rest}")));
            }
            if let Some(root) = project_root {
                if file.is_relative() {
                    let rs = root.to_string_lossy().replace('\\', "/");
                    if let Some(rest) = rs.strip_prefix(&host) {
                        push(
                            &mut out,
                            std::path::PathBuf::from(format!("{cont}{rest}")).join(file),
                        );
                    }
                    if let Some(rest) = rs.strip_prefix(&cont) {
                        push(
                            &mut out,
                            std::path::PathBuf::from(format!("{host}{rest}")).join(file),
                        );
                    }
                }
            }
        }
    }
    out
}

/// Source / include-guard text for seed qualification.
///
/// Slim Complete warehouses strip `source`; peek the first 4 KiB of the file so
/// `namespace mozilla` / `mozilla_Mutex_h` still beat a higher-degree twin in `js/`.
/// Tries project-root join + host/container path dialects.
fn block_seed_text(b: &BlockInfo, project_root: Option<&std::path::Path>) -> String {
    if !b.source.trim().is_empty() {
        return b.source.clone();
    }
    use std::io::Read;
    for path in seed_file_open_candidates(&b.file, project_root) {
        if let Ok(f) = std::fs::File::open(&path) {
            let mut buf = String::new();
            if f.take(4096).read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                return buf;
            }
        }
    }
    String::new()
}

/// Portable evidence that `b` belongs to qualified name `parent_name::leaf`.
///
/// Higher = stronger. Does **not** hardcode product trees (xpcom vs js) — uses AST
/// ancestors, path segments, `namespace X`, `X::Leaf`, and `X_Leaf` include guards.
pub fn qualification_evidence(
    b: &BlockInfo,
    parent_name: &str,
    leaf: &str,
    project_root: Option<&std::path::Path>,
) -> i32 {
    let mut score = 0i32;
    if path_has_any_parent_token(&b.file, parent_name) {
        score += 80;
    }
    let text = block_seed_text(b, project_root);
    if text.is_empty() {
        return score;
    }
    let qual = format!("{parent_name}::{leaf}");
    if text.contains(&qual) {
        score += 200;
    }
    // C++ include guards: mozilla_Mutex_h, mozilla_detail_Foo_h
    let guard = format!(
        "{}_{}",
        parent_name.replace("::", "_"),
        leaf
    );
    if text.contains(&guard) {
        score += 160;
    }
    for tok in parent_name.split("::").filter(|t| !t.is_empty()) {
        if text.contains(&format!("namespace {tok}")) {
            score += 150;
        }
        // `using mozilla::Mutex` / friends
        if text.contains(&format!("{tok}::{leaf}")) {
            score += 100;
        }
    }
    score
}

/// True when block is a credible host for `parent::leaf` (path, AST, or source evidence).
pub fn matches_qualified_prefix(
    graph: &code_graph::CodeGraph,
    b: &BlockInfo,
    parent_name: &str,
    leaf: &str,
    project_root: Option<&std::path::Path>,
) -> bool {
    if has_ancestor_named(graph, b.id.clone(), parent_name) {
        return true;
    }
    if path_has_any_parent_token(&b.file, parent_name) {
        return true;
    }
    qualification_evidence(b, parent_name, leaf, project_root) > 0
}

/// Scope-first seed for qualified names (`mozilla::Mutex`, `a::b::Foo`).
///
/// **Hot path:** rank only surgical `scoped` hits named `leaf` that pass qualification
/// (namespace / path / include-guard / source). Never rank bare leaf twins when the
/// agent typed a qualifier — that was the JS `Mutex.h` star-steal.
///
/// **Fallback:** stream `name_index[leaf]` with the same predicate (bounded).
#[allow(dead_code)] // unit tests + external callers; serve uses `_in` with project root
pub fn seed_qualified_symbol<'a>(
    graph: &'a code_graph::CodeGraph,
    scoped: &[&'a BlockInfo],
    symbol: &str,
) -> Option<&'a BlockInfo> {
    seed_qualified_symbol_in(graph, scoped, symbol, None)
}

/// Same as [`seed_qualified_symbol`] with project root for disk peek under Docker mounts.
pub fn seed_qualified_symbol_in<'a>(
    graph: &'a code_graph::CodeGraph,
    scoped: &[&'a BlockInfo],
    symbol: &str,
    project_root: Option<&std::path::Path>,
) -> Option<&'a BlockInfo> {
    let sym = symbol.trim();
    let Some((parent_name, leaf)) = sym.rsplit_once("::") else {
        return None;
    };
    let leaf = leaf.trim();
    let parent_name = parent_name.trim();
    if leaf.is_empty() || parent_name.is_empty() {
        return None;
    }

    let matches_prefix = |b: &BlockInfo| -> bool {
        matches_qualified_prefix(graph, b, parent_name, leaf, project_root)
    };

    // Among qualified hits: stronger evidence first, then homonym ladder (degree, basename).
    let rank_qualified = |cands: Vec<&'a BlockInfo>| -> Option<&'a BlockInfo> {
        if cands.is_empty() {
            return None;
        }
        let mut best_ev = i32::MIN;
        let mut top: Vec<&BlockInfo> = Vec::new();
        for b in cands {
            let ev = qualification_evidence(b, parent_name, leaf, project_root);
            if ev > best_ev {
                best_ev = ev;
                top.clear();
                top.push(b);
            } else if ev == best_ev {
                top.push(b);
            }
        }
        pick_best_homonym_on_graph(graph, top)
    };

    // --- Scope-first: surgical set already O(leaf hits ∩ path) ---
    let leaf_scoped: Vec<&BlockInfo> = scoped
        .iter()
        .copied()
        .filter(|b| b.name == leaf)
        .collect();
    if !leaf_scoped.is_empty() {
        let with_ns: Vec<&BlockInfo> = leaf_scoped
            .iter()
            .copied()
            .filter(|b| matches_prefix(b))
            .collect();
        if let Some(s) = rank_qualified(with_ns) {
            return Some(s);
        }
        // **No** unfiltered leaf_scoped fallback — that crowned js Mutex over mozilla::Mutex.
    }

    // --- Safe global fallback: predicate stream, not random top-K ---
    const MAX_SCAN: usize = 2_048;
    const MAX_ACCEPT: usize = 32;
    let locs = graph.locations_for_name(leaf);
    if locs.is_empty() {
        return None;
    }
    let mut accepted: Vec<&BlockInfo> = Vec::with_capacity(MAX_ACCEPT);
    let mut scanned = 0usize;
    for loc in locs {
        if scanned >= MAX_SCAN {
            break;
        }
        scanned += 1;
        let Some(b) = graph.nodes.get(&loc.id) else {
            continue;
        };
        if b.name != leaf {
            continue;
        }
        if !matches_prefix(b) {
            continue;
        }
        accepted.push(b);
        if accepted.len() >= MAX_ACCEPT {
            break;
        }
    }
    rank_qualified(accepted)
}

/// Seed rank with an injected in-degree function (tests pass a map; serve uses graph reverse).
/// Pass `|_| 0` when topology is unavailable (unit tests without a warehouse).
///
/// **Ladder (high → low)** — see module docs; do not reorder without dual-stack keepers:
/// basename hard filter → role tier (±shell/ctor) → C body pref → in-degree if both basename
/// → path context → app path priority → non-test → in-degree → span → source len.
pub fn pick_best_homonym_with_in_degree<'a>(
    candidates: impl IntoIterator<Item = &'a BlockInfo>,
    in_degree: impl Fn(&BlockInfo) -> usize,
) -> Option<&'a BlockInfo> {
    let mut filtered = filter_seed_candidates(candidates.into_iter().collect());
    if filtered.is_empty() {
        return None;
    }
    // B0 hard rule: basename match never loses to a same-named helper in another file
    // (OwningNonNull.h `nsCOMPtr` free fn vs nsCOMPtr.h — universal Type.h convention).
    if filtered.iter().any(|b| has_basename_symbol_match(b)) {
        filtered.retain(|b| has_basename_symbol_match(b));
    }
    filtered.into_iter().max_by(|a, b| {
        // Tier with forward-shell + ctor demotion (Constructor Trap).
        let ta = seed_role_tier(&a.kind)
            + if is_forward_or_shell_seed(a) { -50 } else { 0 }
            + if is_likely_constructor_or_destructor(a) {
                -80
            } else {
                0
            };
        let tb = seed_role_tier(&b.kind)
            + if is_forward_or_shell_seed(b) { -50 } else { 0 }
            + if is_likely_constructor_or_destructor(b) {
                -80
            } else {
                0
            };
        let ma = filename_matches_symbol(a);
        let mb = filename_matches_symbol(b);
        let in_a = in_degree(a);
        let in_b = in_degree(b);
        let ca = homonym_context_score(a);
        let cb = homonym_context_score(b);
        let pa = application_path_priority(&a.file.to_string_lossy());
        let pb = application_path_priority(&b.file.to_string_lossy());
        let na = !is_testish_seed_block(a);
        let nb = !is_testish_seed_block(b);
        // C/C++: prefer definition body over header prototype (lang-owned score).
        let c_a = code_graph::c_impl_preference_score(a);
        let c_b = code_graph::c_impl_preference_score(b);
        // Prefer fuller definitions (body length) over empty shells / thin ctors
        let la = a.source.len().min(50_000);
        let lb = b.source.len().min(50_000);
        // Prefer larger line span (class body vs one-line ctor)
        let spa = a.end_line.saturating_sub(a.start_line);
        let spb = b.end_line.saturating_sub(b.start_line);

        // Basename → role → C def/body → path context → **directed in-degree** → span.
        // No global hub `score` (celebrity steal).
        //
        // **Basename-tied gravity:** when both match Type.h basename (e.g. two Mutex.h),
        // prefer CALL reverse in-degree *before* application_path_priority. Otherwise
        // `src/` (priority 150) steals over product trees like `xpcom/` even when the
        // latter is structural backbone. Portable topology — not repo path spines.
        // When basenames differ, path heuristics still run before late in-degree so
        // torch public spines can beat csrc when degrees are cold/unset.
        ma.cmp(&mb)
            .then(ta.cmp(&tb))
            .then(c_a.cmp(&c_b))
            .then_with(|| {
                if ma > 0 && mb > 0 {
                    in_a.cmp(&in_b)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then(ca.cmp(&cb))
            .then(pa.cmp(&pb))
            .then(na.cmp(&nb))
            .then(in_a.cmp(&in_b))
            .then(spa.cmp(&spb))
            .then(la.cmp(&lb))
    })
}
