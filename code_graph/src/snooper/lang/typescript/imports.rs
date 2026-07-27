//! Relative + common path-alias ES module imports → same-lang def edges (TS/JS drawer).
//!
//! Complements call_expression edges: `import { foo } from './bar'` and
//! `import useAuth from '@/hooks/useAuth'` (tsconfig `paths` / `@/*` → `src/*`)
//! link a host block in the importer to defs in the resolved file when present.
//! Bare package imports (`react`, `@tanstack/...`) stay unresolved.
//!
//! **L1.2 Cut 2:** barrel re-export walk via [`super::exports::ExportTable`]
//! (depth ≤ 8, named wins over star, warehouse-only paths, silence on cycle/ambiguity).
//!
//! **L1.2 Cut 3:** import-bound call / JSX — if a call or `<Button />` uses a local
//! import binding, resolve the **export name** through the same walk to the terminus
//! def (not bare global homonyms). Member expressions (`Form.Item`) stay non-bound.

use super::exports::{parse_export_table, ExportTable, ExportTarget};
use crate::snooper::model::{BlockInfo, CodeGraph, Id};
use crate::snooper::project_paths::ProjectPaths;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tree_sitter::{Query, QueryCursor, StreamingIterator};

/// Max re-export hops (barrel chains). Beyond this → silence.
const REEXPORT_DEPTH_CAP: usize = 8;

/// Call / JSX name captures for import-bound resolution (subset of CALL_QUERY).
const IMPORT_BOUND_CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @name
)
(new_expression
  constructor: (identifier) @name
)
(jsx_element
  open_tag: (jsx_opening_element
    name: (identifier) @name
  )
)
(jsx_self_closing_element
  name: (identifier) @name
)
"#;

/// Build import edges for all TS/JS files in the warehouse.
pub fn link_relative_imports(
    graph: &CodeGraph,
    project_root: Option<&Path>,
) -> Vec<(Id, Id)> {
    let pp = project_root.map(ProjectPaths::new);
    let aliases = project_root
        .map(load_tsconfig_path_aliases)
        .unwrap_or_default();
    // file_key (normalized) → name → Id (prefer function/class)
    let mut by_file_name: HashMap<String, HashMap<String, Id>> = HashMap::new();
    let mut blocks_by_file: HashMap<String, Vec<&BlockInfo>> = HashMap::new();

    for b in graph.nodes.values() {
        if !is_ts_js(b) {
            continue;
        }
        let key = file_key(&b.file);
        blocks_by_file.entry(key.clone()).or_default().push(b);
        if is_exportable_def(b) && !b.name.is_empty() {
            let e = by_file_name.entry(key).or_default();
            let score = def_score(b);
            match e.get(&b.name) {
                Some(prev_id) => {
                    if let Some(prev) = graph.nodes.get(prev_id) {
                        if def_score(prev) < score {
                            e.insert(b.name.clone(), b.id.clone());
                        }
                    }
                }
                None => {
                    e.insert(b.name.clone(), b.id.clone());
                }
            }
        }
    }

    // Per-file export tables (Cut 1 parser) — one pass over sources.
    let mut export_tables: HashMap<String, ExportTable> = HashMap::new();
    for (file, blocks) in &blocks_by_file {
        if let Some(src) = file_source(file, blocks, pp.as_ref()) {
            export_tables.insert(file.clone(), parse_export_table(&src));
        }
    }

    let mut edges: Vec<(Id, Id)> = Vec::new();
    let mut import_bound_calls = 0usize;
    for (file, blocks) in &blocks_by_file {
        let src = file_source(file, blocks, pp.as_ref());
        let Some(src) = src else {
            continue;
        };
        let imports = parse_module_imports(&src);
        if imports.is_empty() {
            continue;
        }
        let parent = Path::new(file).parent().unwrap_or(Path::new(""));

        // --- Cut 2: module-level import → def (host block) ---
        if let Some(host) = pick_host_block(blocks) {
            for imp in &imports {
                let Some(resolved) =
                    resolve_import_module(parent, &imp.module_path, project_root, &aliases)
                else {
                    continue;
                };
                for nb in &imp.names {
                    let mut visited = HashSet::new();
                    if let Some(tid) = resolve_exported_name(
                        &nb.export,
                        &resolved,
                        0,
                        &mut visited,
                        &by_file_name,
                        &export_tables,
                        project_root,
                        &aliases,
                    ) {
                        if tid != host.id {
                            edges.push((host.id.clone(), tid));
                        }
                    }
                }
                if let Some(ref def) = imp.default_name {
                    let mut visited = HashSet::new();
                    if let Some(tid) = resolve_exported_name(
                        def,
                        &resolved,
                        0,
                        &mut visited,
                        &by_file_name,
                        &export_tables,
                        project_root,
                        &aliases,
                    ) {
                        if tid != host.id {
                            edges.push((host.id.clone(), tid));
                        }
                    } else if let Some(stem) = Path::new(&resolved)
                        .file_stem()
                        .and_then(|s| s.to_str())
                    {
                        let mut visited = HashSet::new();
                        if let Some(tid) = resolve_exported_name(
                            stem,
                            &resolved,
                            0,
                            &mut visited,
                            &by_file_name,
                            &export_tables,
                            project_root,
                            &aliases,
                        ) {
                            if tid != host.id {
                                edges.push((host.id.clone(), tid));
                            }
                        }
                    }
                }
            }
        }

        // --- Cut 3: import-bound call / JSX at true call sites ---
        let local_map = local_import_bindings(&imports);
        if !local_map.is_empty() {
            let n = collect_import_bound_call_edges(
                file,
                &src,
                blocks,
                &local_map,
                parent,
                &by_file_name,
                &export_tables,
                project_root,
                &aliases,
                &mut edges,
            );
            import_bound_calls += n;
        }
    }

    edges.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.as_str().cmp(b.1.as_str())));
    edges.dedup();
    if import_bound_calls > 0 {
        println!(
            "⚡ TS/JS import-bound call/JSX edges: {} (before dedup merge)",
            import_bound_calls
        );
    }
    edges
}

/// Local binding → (module_path, export_name in that module).
fn local_import_bindings(imports: &[RelativeImport]) -> HashMap<String, (String, String)> {
    let mut m = HashMap::new();
    for imp in imports {
        for nb in &imp.names {
            m.insert(
                nb.local.clone(),
                (imp.module_path.clone(), nb.export.clone()),
            );
        }
        if let Some(ref def) = imp.default_name {
            // Default import: local name often equals component; resolve export under module.
            m.insert(def.clone(), (imp.module_path.clone(), def.clone()));
        }
    }
    m
}

/// For each caller block, link import-bound bare call / JSX names to re-export termini.
fn collect_import_bound_call_edges(
    _file: &str,
    source: &str,
    blocks: &[&BlockInfo],
    local_map: &HashMap<String, (String, String)>,
    parent: &Path,
    by_file_name: &HashMap<String, HashMap<String, Id>>,
    export_tables: &HashMap<String, ExportTable>,
    project_root: Option<&Path>,
    aliases: &[PathAlias],
    edges: &mut Vec<(Id, Id)>,
) -> usize {
    let Ok(query) = Query::new(
        &tree_sitter_typescript::LANGUAGE_TSX.into(),
        IMPORT_BOUND_CALL_QUERY,
    ) else {
        return 0;
    };
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .is_err()
    {
        return 0;
    }
    let Some(tree) = parser.parse(source, None) else {
        return 0;
    };

    let mut cursor = QueryCursor::new();
    let root = tree.root_node();
    let mut name_nodes = Vec::new();
    {
        let mut matches = cursor.matches(&query, root, source.as_bytes());
        while let Some(mat) = matches.next() {
            for c in mat.captures {
                if c.index == 0 {
                    name_nodes.push(c.node);
                }
            }
        }
    }

    let callers: Vec<&&BlockInfo> = blocks
        .iter()
        .filter(|b| is_caller_kind(b))
        .collect();
    if callers.is_empty() || name_nodes.is_empty() {
        return 0;
    }

    let mut added = 0usize;
    for block in callers {
        let (bs, be) = (block.start_byte, block.end_byte);
        for node in name_nodes
            .iter()
            .filter(|n| n.start_byte() >= bs && n.end_byte() <= be)
        {
            let name = &source[node.start_byte()..node.end_byte()];
            let name = name.trim();
            if name.is_empty() || name.contains('.') {
                continue;
            }
            // JSX DOM intrinsics (lowercase) — never import-bound.
            if name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                continue;
            }
            let Some((mod_path, export_name)) = local_map.get(name) else {
                continue; // not an import binding — leave to QueryOnly stream edges
            };
            let Some(resolved) =
                resolve_import_module(parent, mod_path, project_root, aliases)
            else {
                continue;
            };
            let mut visited = HashSet::new();
            let Some(tid) = resolve_exported_name(
                export_name,
                &resolved,
                0,
                &mut visited,
                by_file_name,
                export_tables,
                project_root,
                aliases,
            ) else {
                continue;
            };
            if tid != block.id {
                edges.push((block.id.clone(), tid));
                added += 1;
            }
        }
    }
    added
}

fn is_caller_kind(b: &BlockInfo) -> bool {
    let k = b.kind.to_ascii_lowercase();
    k.contains("function_declaration")
        || k.contains("method_definition")
        || k.contains("arrow_function")
        || k.contains("class_declaration")
        || k.contains("variable_declarator") // const Page = () => <Button />
}

/// Resolve `name` starting at a module path, walking re-exports (named then star).
///
/// Honesty:
/// - depth > [`REEXPORT_DEPTH_CAP`] → None
/// - cycle (same file key) → None
/// - bare package / unresolvable path → None
/// - two star paths yield different Ids → None (ambiguity)
fn resolve_exported_name(
    name: &str,
    module_resolved: &Path,
    depth: usize,
    visited: &mut HashSet<String>,
    by_file_name: &HashMap<String, HashMap<String, Id>>,
    export_tables: &HashMap<String, ExportTable>,
    project_root: Option<&Path>,
    aliases: &[PathAlias],
) -> Option<Id> {
    if depth > REEXPORT_DEPTH_CAP || name.is_empty() {
        return None;
    }
    let file = pick_module_file(module_resolved, by_file_name, export_tables)?;
    if !visited.insert(file.clone()) {
        return None; // cycle
    }

    // 1. Def lives in this file (true terminus — e.g. Button.tsx).
    if let Some(id) = by_file_name.get(&file).and_then(|m| m.get(name)) {
        return Some(id.clone());
    }

    let table = export_tables.get(&file)?;
    let parent = Path::new(&file).parent().unwrap_or(Path::new(""));

    // 2. Named export entry wins over star.
    if let Some(target) = table.named_exports.get(name) {
        match target {
            ExportTarget::Local => {
                // Declared local but no exportable def Id (type-only / stripped) → silence.
                return None;
            }
            ExportTarget::Named(mod_path, orig) => {
                if !is_resolvable_module_spec(mod_path) {
                    return None;
                }
                let next =
                    resolve_import_module(parent, mod_path, project_root, aliases)?;
                return resolve_exported_name(
                    orig,
                    &next,
                    depth + 1,
                    visited,
                    by_file_name,
                    export_tables,
                    project_root,
                    aliases,
                );
            }
        }
    }

    // 3. Star re-exports (order as written). First hit wins; second distinct Id → silence.
    let mut found: Option<Id> = None;
    for star_path in &table.star_exports {
        if !is_resolvable_module_spec(star_path) {
            continue;
        }
        let Some(next) = resolve_import_module(parent, star_path, project_root, aliases) else {
            continue;
        };
        // Clone visit set per star so sibling barrels still resolve; cycles stay within a branch.
        let mut branch_visited = visited.clone();
        if let Some(id) = resolve_exported_name(
            name,
            &next,
            depth + 1,
            &mut branch_visited,
            by_file_name,
            export_tables,
            project_root,
            aliases,
        ) {
            if let Some(ref prev) = found {
                if prev != &id {
                    return None; // ambiguous multi-star
                }
            } else {
                found = Some(id);
            }
        }
    }
    found
}

/// Pick warehouse file key for a resolved module path (extension / index variants).
fn pick_module_file(
    resolved: &Path,
    by_file_name: &HashMap<String, HashMap<String, Id>>,
    export_tables: &HashMap<String, ExportTable>,
) -> Option<String> {
    for c in module_file_candidates(resolved) {
        if by_file_name.contains_key(&c) || export_tables.contains_key(&c) {
            return Some(c);
        }
    }
    None
}

/// One named import binding (`import { Foo as Bar }` → local Bar, export Foo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedBinding {
    pub local: String,
    pub export: String,
}

#[derive(Debug)]
pub(crate) struct RelativeImport {
    pub module_path: String,
    /// Named bindings (local identifier used in this file → export name in module).
    pub names: Vec<NamedBinding>,
    pub default_name: Option<String>,
}

/// True if this import specifier is resolvable in-warehouse (relative or `@/` alias).
fn is_resolvable_module_spec(spec: &str) -> bool {
    spec.starts_with('.') || spec.starts_with("@/") || spec.starts_with("~/")
}

/// `import { a, b as c } from './x'` / `import Foo from '../y'` / `import useAuth from '@/hooks/useAuth'`
/// Joins simple multi-line imports (brace list spanning lines before `from`).
pub(crate) fn parse_module_imports(src: &str) -> Vec<RelativeImport> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for line in src.lines() {
        let t = line.trim();
        if buf.is_empty() {
            if !t.starts_with("import ") {
                continue;
            }
            buf.push_str(t);
        } else {
            buf.push(' ');
            buf.push_str(t);
        }
        // Wait until we see ` from ` and a quote after it (complete statement-ish).
        if !buf.contains(" from ") {
            // Cap runaway multi-line
            if buf.len() > 2000 {
                buf.clear();
            }
            continue;
        }
        let t = buf.trim().trim_end_matches(';').to_string();
        buf.clear();
        let Some(from_idx) = t.rfind(" from ") else {
            continue;
        };
        // "import " is 7 bytes; skip malformed joins (fixture soup).
        if from_idx < 7 {
            continue;
        }
        let Some(head) = t.get(7..from_idx) else {
            continue;
        };
        let head = head.trim();
        // strip `type ` after import
        let head = head.strip_prefix("type ").unwrap_or(head).trim();
        let Some(mut rest) = t.get(from_idx + 6..).map(str::trim) else {
            continue;
        };
        rest = rest.trim_end_matches(';').trim();
        // drop trailing comments
        if let Some(i) = rest.find("//") {
            rest = rest.get(..i).unwrap_or(rest).trim();
        }
        let module_path = strip_quotes(rest);
        if !is_resolvable_module_spec(&module_path) {
            continue; // package import — skip
        }

        let mut names = Vec::new();
        let mut default_name = None;

        // default + named: Foo, { a, b }
        // Brace pairing: first `}` in `head` may sit *before* `{` (vite SSR snapshot
        // fixtures glom `import` into test strings). Always pair with the closer
        // after the opener — never slice when begin > end.
        if let Some(brace) = head.find('{') {
            let before = head.get(..brace).unwrap_or("").trim().trim_end_matches(',').trim();
            if !before.is_empty() && !before.starts_with('*') {
                let def = before.split_whitespace().next().unwrap_or("").trim();
                if is_ident(def) {
                    default_name = Some(def.to_string());
                }
            }
            let after_open = brace + 1;
            if let Some(rel_end) = head.get(after_open..).and_then(|s| s.find('}')) {
                let end = after_open + rel_end;
                if let Some(inner) = head.get(after_open..end) {
                    for part in inner.split(',') {
                        let part = part.trim();
                        if part.is_empty() {
                            continue;
                        }
                        // strip `type ` for type-only named imports
                        let part = part.strip_prefix("type ").unwrap_or(part).trim();
                        if part.is_empty() {
                            continue;
                        }
                        // `a as b` → local b, export a; bare `a` → local=export=a
                        if let Some((export, local)) = split_as_binding(part) {
                            if is_ident(export) && is_ident(local) {
                                names.push(NamedBinding {
                                    local: local.to_string(),
                                    export: export.to_string(),
                                });
                            }
                        } else if is_ident(part) {
                            names.push(NamedBinding {
                                local: part.to_string(),
                                export: part.to_string(),
                            });
                        }
                    }
                }
            }
        } else if head.starts_with('*') {
            // import * as ns — no named symbols (Cut 3 does not bind ns.Foo)
        } else {
            // import Foo from './x'
            let def = head.split(',').next().unwrap_or(head).trim();
            let def = def.split_whitespace().next().unwrap_or("").trim();
            if is_ident(def) {
                default_name = Some(def.to_string());
            }
        }

        if !names.is_empty() || default_name.is_some() {
            out.push(RelativeImport {
                module_path,
                names,
                default_name,
            });
        }
    }
    out
}

fn split_as_binding(part: &str) -> Option<(&str, &str)> {
    let mut bits = part.split_whitespace();
    let left = bits.next()?;
    if bits.next()? != "as" {
        return None;
    }
    let right = bits.next()?;
    if bits.next().is_some() {
        return None;
    }
    Some((left, right))
}

/// One tsconfig `paths` mapping: pattern prefix → target under config dir.
#[derive(Debug, Clone)]
struct PathAlias {
    /// e.g. `@/`
    from_prefix: String,
    /// e.g. `frontend/src/` (repo-relative when config dir is frontend/)
    to_prefix: String,
}

fn load_tsconfig_path_aliases(project_root: &Path) -> Vec<PathAlias> {
    // Cheap: scan shallow for tsconfig*.json (depth-limited).
    let mut out = Vec::new();
    let mut stack = vec![project_root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        if seen > 80 {
            break;
        }
        seen += 1;
        for name in ["tsconfig.json", "tsconfig.app.json", "tsconfig.base.json"] {
            let p = dir.join(name);
            if p.is_file() {
                if let Some(mut aliases) = parse_tsconfig_paths(&p, project_root) {
                    out.append(&mut aliases);
                }
            }
        }
        // only one level of children for package monorepos (frontend/, packages/*)
        if dir == project_root {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if p.is_dir() {
                        let n = ent.file_name().to_string_lossy().to_string();
                        if n.starts_with('.') || n == "node_modules" || n == "target" {
                            continue;
                        }
                        stack.push(p);
                    }
                }
            }
        }
    }
    // Always offer common Vite/CRA default when nothing found.
    if out.is_empty() {
        for candidate in ["src/", "frontend/src/", "app/src/"] {
            let abs = project_root.join(candidate);
            if abs.is_dir() {
                let rel = normalize_rel(candidate);
                out.push(PathAlias {
                    from_prefix: "@/".into(),
                    to_prefix: rel,
                });
                break;
            }
        }
    }
    out
}

fn normalize_rel(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = s.trim_start_matches("./");
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
}

fn parse_tsconfig_paths(tsconfig: &Path, project_root: &Path) -> Option<Vec<PathAlias>> {
    let text = std::fs::read_to_string(tsconfig).ok()?;
    // Strip // comments lightly for broken JSON
    let cleaned: String = text
        .lines()
        .map(|l| {
            if let Some(i) = l.find("//") {
                // keep urls
                if l[..i].contains(':') {
                    l
                } else {
                    &l[..i]
                }
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let v: serde_json::Value = serde_json::from_str(&cleaned).ok()?;
    let paths = v
        .pointer("/compilerOptions/paths")?
        .as_object()?;
    let config_dir = tsconfig.parent().unwrap_or(project_root);
    let mut out = Vec::new();
    for (pat, targets) in paths {
        let Some(arr) = targets.as_array() else {
            continue;
        };
        let Some(target0) = arr.first().and_then(|x| x.as_str()) else {
            continue;
        };
        // Only simple `prefix/*` → `dir/*` patterns
        let from = pat.trim();
        let to = target0.trim();
        let (from_prefix, to_star) = if let Some(p) = from.strip_suffix("/*") {
            (format!("{p}/"), to.strip_suffix("/*").unwrap_or(to))
        } else if from.ends_with('*') {
            continue;
        } else {
            (from.to_string(), to)
        };
        // Resolve to_prefix relative to config dir, then repo-relative
        let to_path = config_dir.join(to_star.trim_start_matches("./"));
        let to_rel = pathdiff_repo(project_root, &to_path).unwrap_or_else(|| {
            normalize_rel(&to_path.to_string_lossy())
        });
        out.push(PathAlias {
            from_prefix: if from_prefix.ends_with('/') {
                from_prefix
            } else {
                format!("{from_prefix}/")
            },
            to_prefix: normalize_rel(&to_rel),
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn pathdiff_repo(root: &Path, abs: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let abs = if abs.exists() {
        abs.canonicalize().ok()?
    } else {
        abs.to_path_buf()
    };
    let rel = abs.strip_prefix(&root).ok()?;
    Some(normalize_rel(&rel.to_string_lossy()))
}

fn resolve_import_module(
    parent: &Path,
    spec: &str,
    project_root: Option<&Path>,
    aliases: &[PathAlias],
) -> Option<PathBuf> {
    if spec.starts_with('.') {
        return Some(resolve_relative(parent, spec));
    }
    for a in aliases {
        if let Some(rest) = spec.strip_prefix(&a.from_prefix) {
            let joined = format!("{}{}", a.to_prefix, rest);
            return Some(PathBuf::from(joined));
        }
        // also `@foo` without trailing when from is `@/`
    }
    // Fallback: `@/x` → `src/x` under project root if present
    if let Some(rest) = spec.strip_prefix("@/") {
        if let Some(root) = project_root {
            for base in ["src", "frontend/src", "app/src"] {
                let p = root.join(base).join(rest);
                if p.exists()
                    || p.with_extension("ts").exists()
                    || p.with_extension("tsx").exists()
                {
                    return Some(PathBuf::from(format!("{base}/{rest}")));
                }
            }
        }
        return Some(PathBuf::from(format!("src/{rest}")));
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    // `""` / `''` → empty; single-char `"` alone must not panic on [1..0].
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s.get(1..s.len() - 1).unwrap_or("").to_string()
    } else {
        s.to_string()
    }
}

fn resolve_relative(parent: &Path, spec: &str) -> PathBuf {
    let mut cur = parent.to_path_buf();
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                let _ = cur.pop();
            }
            p => cur.push(p),
        }
    }
    cur
}

fn module_file_candidates(resolved: &Path) -> Vec<String> {
    let base = file_key(resolved);
    let mut out = vec![base.clone()];
    // extensionless imports
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
        out.push(format!("{base}.{ext}"));
    }
    for ext in ["ts", "tsx", "js", "jsx"] {
        out.push(format!("{base}/index.{ext}"));
    }
    // strip accidental double ext
    out.sort();
    out.dedup();
    out
}

fn file_key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn file_source(
    file: &str,
    blocks: &[&BlockInfo],
    pp: Option<&ProjectPaths>,
) -> Option<String> {
    if let Some(paths) = pp {
        let abs = paths.to_abs(Path::new(file));
        if let Ok(s) = std::fs::read_to_string(abs) {
            return Some(s);
        }
    }
    let joined: String = blocks
        .iter()
        .filter(|b| !b.source.is_empty())
        .map(|b| b.source.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn pick_host_block<'a>(blocks: &[&'a BlockInfo]) -> Option<&'a BlockInfo> {
    // Prefer a top-level function/class; else any named def.
    blocks
        .iter()
        .copied()
        .filter(|b| is_exportable_def(b) && !b.name.is_empty())
        .min_by_key(|b| (b.start_line, b.start_byte))
}

fn is_exportable_def(b: &BlockInfo) -> bool {
    if b.name.is_empty() {
        return false;
    }
    let k = b.kind.to_ascii_lowercase();
    k.contains("function_declaration")
        || k.contains("method_definition")
        || k.contains("arrow_function")
        || k.contains("class_declaration")
        || k.contains("interface_declaration")
        // `const Form = FormProvider` / generic const components (shadcn export lists)
        || k.contains("variable_declarator")
        || (k.contains("lexical_declaration") && b.name.chars().next().is_some_and(|c| c.is_uppercase()))
}

fn def_score(b: &BlockInfo) -> i32 {
    let k = b.kind.to_ascii_lowercase();
    let mut s = 0;
    if k.contains("function_declaration") {
        s += 30;
    } else if k.contains("class_declaration") {
        s += 25;
    } else if k.contains("arrow_function") || k.contains("method") {
        s += 20;
    }
    s
}

fn is_ts_js(b: &BlockInfo) -> bool {
    let l = b.lang.to_ascii_lowercase();
    matches!(
        l.as_str(),
        "typescript" | "javascript" | "ts" | "tsx" | "js" | "jsx"
    ) || b
        .file
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "ts" | "tsx" | "js" | "jsx" | "svelte" | "mjs" | "cjs"))
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    match c.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' => {
            c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_named_and_default_relative() {
        let src = r#"
import { createProject, runCli as run } from './helpers/createProject'
import Home from '../pages/Home'
import fs from 'fs'
"#;
        let imps = parse_module_imports(src);
        assert_eq!(imps.len(), 2);
        assert_eq!(imps[0].module_path, "./helpers/createProject");
        assert!(imps[0].names.iter().any(|n| n.local == "createProject" && n.export == "createProject"));
        assert!(imps[0].names.iter().any(|n| n.local == "run" && n.export == "runCli"));
        assert_eq!(imps[1].default_name.as_deref(), Some("Home"));
        assert_eq!(imps[1].module_path, "../pages/Home");
    }

    #[test]
    fn parse_at_alias_and_multiline() {
        let src = r#"
import useAuth, {
  isLoggedIn,
} from '@/hooks/useAuth'
import { Form } from '@/components/ui/form'
import React from 'react'
"#;
        let imps = parse_module_imports(src);
        assert!(
            imps.iter().any(|i| i.module_path == "@/hooks/useAuth"
                && i.default_name.as_deref() == Some("useAuth")
                && i.names.iter().any(|n| n.local == "isLoggedIn" && n.export == "isLoggedIn")),
            "{imps:?}"
        );
        assert!(
            imps.iter().any(|i| i.module_path == "@/components/ui/form"
                && i.names.iter().any(|n| n.local == "Form" && n.export == "Form")),
            "{imps:?}"
        );
        assert!(!imps.iter().any(|i| i.module_path == "react"));
    }

    #[test]
    fn resolve_at_alias_with_mapping() {
        let aliases = vec![PathAlias {
            from_prefix: "@/".into(),
            to_prefix: "frontend/src/".into(),
        }];
        let p = resolve_import_module(
            Path::new("frontend/src/routes"),
            "@/hooks/useAuth",
            Some(Path::new("/repo")),
            &aliases,
        )
        .unwrap();
        assert_eq!(file_key(&p), "frontend/src/hooks/useAuth");
    }

    #[test]
    fn resolve_relative_paths() {
        let p = resolve_relative(Path::new("src/app"), "./utils/foo");
        assert_eq!(file_key(&p), "src/app/utils/foo");
        let p2 = resolve_relative(Path::new("src/app"), "../../lib/x");
        assert_eq!(file_key(&p2), "lib/x");
    }

    /// Vite SSR transform tests embed `import` / braces inside snapshot strings.
    /// A naive `find('}')` before pairing with `{` used to panic (begin > end).
    #[test]
    fn parse_survives_vite_snapshot_fixture_soup() {
        let src = r#"
test('x' import 'y' `), ).toMatchInlineSnapshot(` "const __vite_ssr_import_0__ = await __vite_ssr_import__("x"); const __vite_ssr_import_1__ = await __vite_ssr_import__("y");   " `) })
import { createServer } from './server'
import } weird { broken from './nope'
import Foo from './ok'
"#;
        let imps = parse_module_imports(src);
        assert!(
            imps.iter().any(|i| i.module_path == "./server"
                && i.names.iter().any(|n| n.export == "createServer")),
            "expected real import; got {imps:?}"
        );
        assert!(
            imps.iter()
                .any(|i| i.module_path == "./ok" && i.default_name.as_deref() == Some("Foo")),
            "expected default import; got {imps:?}"
        );
        // Malformed brace soup must not panic and must not invent bogus names.
        assert!(
            !imps.iter().any(|i| i.module_path == "./nope"),
            "malformed must be skipped; got {imps:?}"
        );
    }

    #[test]
    fn strip_quotes_empty_and_single_char() {
        assert_eq!(strip_quotes("\"\""), "");
        assert_eq!(strip_quotes("''"), "");
        assert_eq!(strip_quotes("\""), "\"");
        assert_eq!(strip_quotes("'a'"), "a");
        assert_eq!(strip_quotes("\"./x\""), "./x");
    }

    /// Barrel: `export * from './Button'` → resolve Button to Button.tsx def.
    #[test]
    fn walk_star_reexport_to_terminus() {
        let button_id = Id::from("src/components/Button.tsx:function_declaration:abcd1234");
        let mut by_file: HashMap<String, HashMap<String, Id>> = HashMap::new();
        by_file.insert(
            "src/components/Button.tsx".into(),
            HashMap::from([("Button".into(), button_id.clone())]),
        );
        let mut tables = HashMap::new();
        tables.insert(
            "src/components/index.ts".into(),
            parse_export_table("export * from './Button'\n"),
        );
        tables.insert(
            "src/components/Button.tsx".into(),
            parse_export_table("export function Button() {}\n"),
        );
        let mut visited = HashSet::new();
        let got = resolve_exported_name(
            "Button",
            Path::new("src/components"),
            0,
            &mut visited,
            &by_file,
            &tables,
            None,
            &[],
        );
        assert_eq!(got.as_ref(), Some(&button_id));
    }

    /// Named re-export wins over a colliding star.
    #[test]
    fn walk_named_wins_over_star() {
        let from_button = Id::from("src/Button.tsx:function_declaration:btn00001");
        let from_text = Id::from("src/Text.tsx:function_declaration:btn00002");
        let mut by_file: HashMap<String, HashMap<String, Id>> = HashMap::new();
        by_file.insert(
            "src/Button.tsx".into(),
            HashMap::from([("Button".into(), from_button.clone())]),
        );
        by_file.insert(
            "src/Text.tsx".into(),
            HashMap::from([("Button".into(), from_text.clone())]),
        );
        let mut tables = HashMap::new();
        tables.insert(
            "src/index.ts".into(),
            parse_export_table(
                "export * from './Text'\nexport { Button } from './Button'\n",
            ),
        );
        tables.insert(
            "src/Button.tsx".into(),
            parse_export_table("export function Button() {}\n"),
        );
        tables.insert(
            "src/Text.tsx".into(),
            parse_export_table("export function Button() {}\n"),
        );
        let mut visited = HashSet::new();
        let got = resolve_exported_name(
            "Button",
            Path::new("src"),
            0,
            &mut visited,
            &by_file,
            &tables,
            None,
            &[],
        );
        assert_eq!(
            got.as_ref(),
            Some(&from_button),
            "named re-export must prefer ./Button over star ./Text"
        );
    }

    /// Two stars both defining the same name with different Ids → silence.
    #[test]
    fn walk_ambiguous_multi_star_stays_silent() {
        let a = Id::from("src/A.tsx:function_declaration:aaa00001");
        let b = Id::from("src/B.tsx:function_declaration:bbb00001");
        let mut by_file: HashMap<String, HashMap<String, Id>> = HashMap::new();
        by_file.insert(
            "src/A.tsx".into(),
            HashMap::from([("Widget".into(), a)]),
        );
        by_file.insert(
            "src/B.tsx".into(),
            HashMap::from([("Widget".into(), b)]),
        );
        let mut tables = HashMap::new();
        tables.insert(
            "src/index.ts".into(),
            parse_export_table("export * from './A'\nexport * from './B'\n"),
        );
        tables.insert("src/A.tsx".into(), parse_export_table("export function Widget() {}\n"));
        tables.insert("src/B.tsx".into(), parse_export_table("export function Widget() {}\n"));
        let mut visited = HashSet::new();
        let got = resolve_exported_name(
            "Widget",
            Path::new("src"),
            0,
            &mut visited,
            &by_file,
            &tables,
            None,
            &[],
        );
        assert_eq!(got, None, "ambiguous multi-star must not invent a winner");
    }

    /// Cut 3: `<Button />` with import from barrel resolves to terminus, not silence.
    #[test]
    fn import_bound_jsx_through_barrel() {
        let button_id = Id::from("src/components/Button.tsx:function_declaration:btn00001");
        let app_id = Id::from("src/App.tsx:function_declaration:app00001");
        let mut by_file: HashMap<String, HashMap<String, Id>> = HashMap::new();
        by_file.insert(
            "src/components/Button.tsx".into(),
            HashMap::from([("Button".into(), button_id.clone())]),
        );
        by_file.insert(
            "src/App.tsx".into(),
            HashMap::from([("App".into(), app_id.clone())]),
        );
        let mut tables = HashMap::new();
        tables.insert(
            "src/components/index.ts".into(),
            parse_export_table("export * from './Button'\n"),
        );
        tables.insert(
            "src/components/Button.tsx".into(),
            parse_export_table("export function Button() {}\n"),
        );
        tables.insert("src/App.tsx".into(), parse_export_table("export function App() {}\n"));

        let app_src = r#"
import { Button } from './components'
export function App() {
  return <Button />
}
"#;
        let app_block = BlockInfo {
            id: app_id.clone(),
            name: "App".into(),
            file: PathBuf::from("src/App.tsx"),
            kind: "function_declaration".into(),
            lang: "typescript".into(),
            start_line: 3,
            end_line: 5,
            start_byte: app_src.find("export function App").unwrap_or(0),
            end_byte: app_src.len(),
            parent_id: None,
            children: vec![],
            content_hash: "app00001".into(),
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: Default::default(),
            is_highly_connected: false,
        };
        let blocks = [&app_block];
        let imports = parse_module_imports(app_src);
        let local_map = local_import_bindings(&imports);
        let mut edges = Vec::new();
        let n = collect_import_bound_call_edges(
            "src/App.tsx",
            app_src,
            &blocks,
            &local_map,
            Path::new("src"),
            &by_file,
            &tables,
            None,
            &[],
            &mut edges,
        );
        assert!(n >= 1, "expected import-bound JSX edge count; edges={edges:?}");
        assert!(
            edges.iter().any(|(f, t)| f == &app_id && t == &button_id),
            "App → Button terminus via barrel; edges={edges:?}"
        );
    }

    /// Unimported homonym must not get an import-bound edge (silence on binding miss).
    #[test]
    fn unimported_name_not_import_bound() {
        let other = Id::from("src/other.ts:function_declaration:oth00001");
        let mut by_file: HashMap<String, HashMap<String, Id>> = HashMap::new();
        by_file.insert(
            "src/other.ts".into(),
            HashMap::from([("Helper".into(), other.clone())]),
        );
        let tables = HashMap::new();
        let src = r#"
export function App() {
  return <Helper />
}
"#;
        // Block spans whole function so JSX is inside.
        let app_id = Id::from("src/App.tsx:function_declaration:app00002");
        let app_block = BlockInfo {
            id: app_id.clone(),
            name: "App".into(),
            file: PathBuf::from("src/App.tsx"),
            kind: "function_declaration".into(),
            lang: "typescript".into(),
            start_line: 2,
            end_line: 4,
            start_byte: 0,
            end_byte: src.len(),
            parent_id: None,
            children: vec![],
            content_hash: "app00002".into(),
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: Default::default(),
            is_highly_connected: false,
        };
        let blocks = [&app_block];
        let local_map = local_import_bindings(&parse_module_imports(src));
        assert!(local_map.is_empty());
        let mut edges = Vec::new();
        let n = collect_import_bound_call_edges(
            "src/App.tsx",
            src,
            &blocks,
            &local_map,
            Path::new("src"),
            &by_file,
            &tables,
            None,
            &[],
            &mut edges,
        );
        assert_eq!(n, 0);
        assert!(edges.is_empty());
    }

    /// Bare package path in export table must not resolve.
    #[test]
    fn walk_bare_package_star_stays_silent() {
        let mut by_file: HashMap<String, HashMap<String, Id>> = HashMap::new();
        by_file.insert(
            "src/index.ts".into(),
            HashMap::new(),
        );
        let mut tables = HashMap::new();
        // parse records the star; walk must refuse non-relative specs
        tables.insert(
            "src/index.ts".into(),
            parse_export_table("export * from 'react'\n"),
        );
        let mut visited = HashSet::new();
        let got = resolve_exported_name(
            "useState",
            Path::new("src"),
            0,
            &mut visited,
            &by_file,
            &tables,
            None,
            &[],
        );
        assert_eq!(got, None);
    }
}
