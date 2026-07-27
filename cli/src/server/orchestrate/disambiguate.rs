//! Homonym disambiguation + repo-relative scope pins (Track T.2).
use crate::server::dto::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Track T.2 — Homonym disambiguation (alts-first; never confident wrong ★)
// ---------------------------------------------------------------------------

/// Env `BUTLER_DISAMBIGUATE=0` disables. Min unique production files (default 3).
pub(crate) fn disambiguate_min_alts() -> usize {
    if matches!(
        std::env::var("BUTLER_DISAMBIGUATE").as_deref(),
        Ok("0") | Ok("false") | Ok("off") | Ok("no")
    ) {
        return usize::MAX;
    }
    std::env::var("BUTLER_DISAMBIGUATE_MIN_ALTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 2)
        .unwrap_or(3)
}

/// Bare short / danger names that commonly collide across a monorepo.
pub(crate) fn is_homonym_risk_name(symbol: &str) -> bool {
    let s = symbol.trim();
    if s.is_empty() || s.contains("::") || s.contains('/') || s.contains('\\') {
        return false;
    }
    const DANGER: &[&str] = &[
        "build", "run", "main", "app", "init", "create", "default", "get", "set", "new",
        "open", "close", "start", "stop", "config", "test", "handler", "index", "load",
        "save", "update", "delete", "render", "parse", "format", "process", "handle",
        "setup", "reset", "clear", "read", "write", "send", "recv", "connect", "server",
        "client", "group", "option", "command", "context", "error", "result", "value",
        "type", "data", "item", "node", "list", "map", "filter", "reduce", "apply",
        // IPC / framework entry points (tauri invoke, etc.)
        "invoke", "plugin", "dispatch", "execute", "call",
    ];
    if DANGER.iter().any(|d| d.eq_ignore_ascii_case(s)) {
        return true;
    }
    s.len() <= 8 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_testish_loc_path(file: &str) -> bool {
    let p = file.replace('\\', "/").to_ascii_lowercase();
    p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("__tests__")
        || p.contains("/benchmark")
        || p.contains("/benches/")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
}

/// Production-ish defs only (not call sites / test / bench trees / statement noise).
///
/// **I9:** never list `let_declaration` / assignment / control-flow as pin targets —
/// pinning those files used to lift ★ to a different function (same mega-homonym).
/// Floor is [`TIER_BINDING`]: const/static/fn/type/impl — not mod shells or NEVER tier.
pub(crate) fn serious_def_locations(locs: &[SymbolLocation]) -> Vec<&SymbolLocation> {
    use crate::server::filters::{seed_role_tier, TIER_BINDING};
    locs.iter()
        .filter(|l| {
            let k = l.kind.to_ascii_lowercase();
            if k.contains("call") {
                return false;
            }
            // Drop let/mod/assignment noise — not pin-worthy Trace seeds.
            if seed_role_tier(&l.kind) < TIER_BINDING {
                return false;
            }
            !is_testish_loc_path(&l.file)
        })
        .collect()
}

/// Multi-file name collision signal (includes lets/mods — not seed-worthy, still multi-loc).
///
/// bevy `app` surfaces as 15× let + 1× mod across 9 files: serious_def count is 0, but
/// high|type-neighborhood without pin is a frankenstein. Collision count forces T.2.
pub(crate) fn collision_alt_locations(locs: &[SymbolLocation]) -> Vec<&SymbolLocation> {
    locs.iter()
        .filter(|l| {
            let k = l.kind.to_ascii_lowercase();
            if k.contains("call") {
                return false;
            }
            !is_testish_loc_path(&l.file)
        })
        .collect()
}

pub(crate) fn serious_alt_file_count(locs: &[SymbolLocation]) -> usize {
    let mut files = std::collections::HashSet::new();
    for l in serious_def_locations(locs) {
        files.insert(l.file.replace('\\', "/"));
    }
    files.len()
}

pub(crate) fn collision_alt_file_count(locs: &[SymbolLocation]) -> usize {
    let mut files = std::collections::HashSet::new();
    for l in collision_alt_locations(locs) {
        files.insert(l.file.replace('\\', "/"));
    }
    files.len()
}

/// Distinct langs among serious defs (ts+rs command twins, py+c++ exports).
pub(crate) fn serious_alt_lang_count(locs: &[SymbolLocation]) -> usize {
    let mut langs = std::collections::HashSet::new();
    for l in serious_def_locations(locs) {
        let lang = l.lang.as_deref().unwrap_or("").trim().to_ascii_lowercase();
        if !lang.is_empty() {
            langs.insert(lang);
        }
    }
    langs.len()
}

/// Pin rows for disambiguate UI: prefer serious defs; fall back to collision hits.
pub(crate) fn pin_locations_for_disambiguate(locs: &[SymbolLocation]) -> Vec<&SymbolLocation> {
    let serious = serious_def_locations(locs);
    if !serious.is_empty() {
        return serious;
    }
    collision_alt_locations(locs)
}

/// T.2 gate: short/danger name + multi-file alts + no single-file scope pin.
///
/// - **Serious defs** (fn/type/binding): ≥ min files → disambiguate (min 2 for risk names).
/// - **Collision** (non-call multi-file, even lets/mods): also forces disambiguate so
///   type_neighborhood / bare-name cannot high|complete a mega-homonym (bevy `app`,
///   tauri `invoke`, click `command`).
/// - Cross-language multi-loc: **2 files** is enough (Tauri `echo` ts+rs).
pub(crate) fn needs_homonym_disambiguation(
    symbol: &str,
    locations: &[SymbolLocation],
    scope_paths: Option<&[String]>,
) -> bool {
    if !is_homonym_risk_name(symbol) {
        return false;
    }
    let mut min = disambiguate_min_alts();
    if min == usize::MAX {
        return false;
    }
    // Risk names: two production files is already a coin-flip ★ (was 3 — missed click).
    min = min.min(2);
    if serious_alt_lang_count(locations) >= 2 {
        min = min.min(2);
    }
    let serious_n = serious_alt_file_count(locations);
    let collision_n = collision_alt_file_count(locations);
    if let Some(scopes) = scope_paths {
        let file_pin = scopes.iter().any(|s| {
            let t = s.trim().trim_end_matches('/');
            t.contains('.')
                && (t.ends_with(".rs")
                    || t.ends_with(".ts")
                    || t.ends_with(".tsx")
                    || t.ends_with(".js")
                    || t.ends_with(".jsx")
                    || t.ends_with(".py")
                    || t.ends_with(".go")
                    || t.ends_with(".c")
                    || t.ends_with(".h")
                    || t.ends_with(".cpp")
                    || t.ends_with(".hpp")
                    || t.ends_with(".svelte"))
        });
        if file_pin {
            return false;
        }
        // Dir pin that already isolates to one collision file → proceed to Trace.
        if serious_n <= 1 && collision_n <= 1 {
            return false;
        }
    }
    serious_n >= min || collision_n >= min
}

/// Repo-relative pin for `scope_paths` (file **or** dir). Never host-absolute.
///
/// Disambiguate agents copy these back on the next call — host paths
/// (`/home/…`) match nothing under the warehouse root and look like a hang.
pub(crate) fn scope_pin_from_display(root: &Path, raw: &str) -> Option<String> {
    let pp = code_graph::ProjectPaths::new(root);
    let mut s = code_graph::snooper::normalize_path(&pp.to_rel(Path::new(raw)).to_string_lossy());
    s = s
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string();
    // Mount env may be unset in unit tests: peel `…/<root_name>/rest` → `rest`
    // e.g. home/user/projects/test_repos/gin/gin.go + root …/gin → gin.go
    if let Some(name) = root.file_name().and_then(|x| x.to_str()) {
        if !name.is_empty() {
            let marker = format!("/{name}/");
            let probe = format!("/{s}");
            if let Some(idx) = probe.rfind(&marker) {
                s = probe[idx + marker.len()..].trim_start_matches('/').to_string();
            }
        }
    }
    if s.is_empty() || s == "." {
        return None;
    }
    let first = s.split('/').filter(|p| !p.is_empty()).next().unwrap_or("");
    // Still host-shaped after peel — refuse (do not re-emit).
    if first.eq_ignore_ascii_case("home")
        || first.eq_ignore_ascii_case("Users")
        || s.starts_with('/')
        || s.contains(":/")
    {
        return None;
    }
    Some(s)
}

/// Homonym / disambiguate pins: file first, then parent dir. Always repo-relative.
pub(crate) fn suggested_scopes_from_locations(
    root: &Path,
    locs: &[&SymbolLocation],
    max: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for l in locs.iter().take(max.saturating_mul(2)) {
        let Some(pin) = scope_pin_from_display(root, &l.file) else {
            continue;
        };
        if seen.insert(pin.clone()) {
            out.push(pin.clone());
        }
        if let Some(parent) = Path::new(&pin).parent() {
            let p = parent.to_string_lossy().replace('\\', "/");
            if !p.is_empty() && p != "." {
                if let Some(scope) = sanitize_scope_prefix(root, &p) {
                    if seen.insert(scope.clone()) {
                        out.push(scope);
                    }
                }
            }
        }
        if out.len() >= max {
            break;
        }
    }
    out.truncate(max);
    out
}

/// Build typed interconnect neighbor rows for the Trace seed (P.4).
///
/// **Do not** apply test/examples path noise here. Dual-stack FFI demos live under
/// `examples/` (L2.1/L2.2 word-count); compressing them away hid real Export bridges

pub(crate) fn sanitize_scope_prefix(root: &Path, raw: &str) -> Option<String> {
    let mut s = code_graph::snooper::normalize_path(raw);
    s = s.trim_start_matches("./").to_string();

    let first = s.split('/').filter(|p| !p.is_empty()).next().unwrap_or("");
    let looks_hostish = s.starts_with('/')
        || first.eq_ignore_ascii_case("home")
        || first.eq_ignore_ascii_case("Users")
        || first.eq_ignore_ascii_case("projects")
        || s.contains(":/");

    if looks_hostish {
        // Strip project root / mounts → warehouse-relative.
        let pp = code_graph::ProjectPaths::new(root);
        let rel = pp.to_rel(Path::new(raw));
        s = code_graph::snooper::normalize_path(&rel.to_string_lossy());
        s = s.trim_start_matches("./").trim_start_matches('/').to_string();
    } else {
        s = s.trim_start_matches('/').to_string();
    }

    if s.is_empty() || s == "." {
        return None;
    }
    let first = s.split('/').filter(|p| !p.is_empty()).next().unwrap_or("");
    // Still host-shaped after strip → drop (never emit home/<user>).
    if first.eq_ignore_ascii_case("home")
        || first.eq_ignore_ascii_case("Users")
        || s.starts_with('/')
        || s.contains(":/")
    {
        return None;
    }
    // Cap depth to 2 segments for a usable working-set prefix.
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    // Single file name (no dir) is useless as a scope.
    if parts.len() == 1 && !raw.contains('/') && !raw.contains('\\') {
        // e.g. "main.rs" alone — skip
        if parts[0].contains('.') {
            return None;
        }
    }
    let mut out = if parts.len() > 2 {
        parts[..2].join("/")
    } else {
        parts.join("/")
    };
    if !out.ends_with('/') {
        out.push('/');
    }
    Some(out)
}

/// Build suggested scopes from file paths (hubs, entries) under `root`.
pub(crate) fn suggested_scopes_from_paths<'a>(
    root: &Path,
    paths: impl IntoIterator<Item = &'a str>,
    max: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        // Prefer parent dir as scope prefix.
        let parent = Path::new(p)
            .parent()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string());
        let Some(scope) = sanitize_scope_prefix(root, &parent) else {
            continue;
        };
        if !out.contains(&scope) {
            out.push(scope);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}
