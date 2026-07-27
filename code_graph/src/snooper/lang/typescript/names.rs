//! TypeScript/JS global name preference for call-edge resolution.
//!
//! Template monorepos (t3, scaffolds) ship many same-named `Home` / `Layout` / `Page`
//! defs. Prefer production `src/` / `app/` over `template/` / `extras/` / tests.

use crate::{BlockInfo, Id};
use std::collections::HashMap;
/// Fill `map` for ambiguous TS/JS names. Does not overwrite unique-map entries.
pub(crate) fn prefer_ambiguous_typescript_names(
    nodes: &HashMap<Id, BlockInfo>,
    map: &mut HashMap<String, Id>,
) {
    let mut by_name: HashMap<&str, Vec<&BlockInfo>> = HashMap::new();
    for b in nodes.values() {
        if !is_ts_js_block(b) || b.name.is_empty() || b.name.len() < 2 {
            continue;
        }
        if !is_def_kind(&b.kind) {
            continue;
        }
        by_name.entry(b.name.as_str()).or_default().push(b);
    }

    for (name, candidates) in by_name {
        if map.contains_key(name) {
            continue;
        }
        if candidates.len() == 1 {
            map.insert(name.to_string(), candidates[0].id.clone());
            continue;
        }
        if let Some(best) = candidates.into_iter().max_by_key(|b| preference_score(b)) {
            map.insert(name.to_string(), best.id.clone());
        }
    }
}

fn is_ts_js_block(b: &BlockInfo) -> bool {
    let l = b.lang.to_ascii_lowercase();
    if matches!(
        l.as_str(),
        "typescript" | "javascript" | "ts" | "tsx" | "js" | "jsx"
    ) {
        return true;
    }
    b.file
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "ts" | "tsx" | "js" | "jsx" | "svelte" | "mjs" | "cjs"))
}

fn is_def_kind(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k.contains("function_declaration")
        || k.contains("method_definition")
        || k.contains("arrow_function")
        || k.contains("class_declaration")
        || k.contains("interface_declaration")
}

fn preference_score(b: &BlockInfo) -> i32 {
    let path = b
        .file
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let mut s = 0i32;

    // Production app surfaces
    if path.contains("/src/") || path.starts_with("src/") {
        s += 40;
    }
    if path.contains("/app/") || path.starts_with("app/") {
        s += 45;
    }
    if path.contains("/pages/") || path.contains("/components/") {
        s += 25;
    }
    if path.contains("/lib/") || path.contains("/server/") {
        s += 20;
    }

    // Scaffold / template noise (t3 create-t3-app layout)
    if path.contains("/template/")
        || path.contains("/templates/")
        || path.contains("/extras/")
        || path.contains("/scaffold/")
        || path.contains("/fixtures/")
        || path.contains("/__mocks__/")
        || path.contains("/.storybook/")
    {
        s -= 100;
    }
    if path.contains("/examples/") || path.contains("/example/") {
        s -= 40;
    }
    if is_testish(&path) {
        s -= 80;
    }
    if path.contains("/node_modules/") || path.contains("/dist/") || path.contains("/.next/") {
        s -= 120;
    }

    // Prefer real function/class over call shells
    let k = b.kind.to_ascii_lowercase();
    if k.contains("function_declaration") || k.contains("class_declaration") {
        s += 20;
    } else if k.contains("method_definition") || k.contains("arrow_function") {
        s += 15;
    }

    // Prefer longer distinctive names slightly
    s += (b.name.len() as i32).min(24) / 4;
    s -= (b.start_line as i32).min(400) / 80;
    s
}

fn is_testish(path: &str) -> bool {
    path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("/__tests__/")
        || path.contains(".test.")
        || path.contains(".spec.")
        || path.contains("_test.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::Id;
    use std::path::PathBuf;

    fn blk(name: &str, file: &str) -> BlockInfo {
        let hash = format!("{name}_{file}");
        let hash = format!("{hash:0<16}");
        BlockInfo {
            id: Id::new(file, "function_declaration", &hash),
            name: name.into(),
            file: PathBuf::from(file),
            kind: "function_declaration".into(),
            lang: "typescript".into(),
            start_line: 1,
            end_line: 10,
            start_byte: 0,
            end_byte: 20,
            parent_id: None,
            children: vec![],
            content_hash: hash,
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            score: 0.0,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            usages: vec![],
            external_crates: Default::default(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn prefers_app_over_template_home() {
        let a = blk("Home", "cli/template/extras/src/pages/index/with-tw.tsx");
        let b = blk("Home", "src/app/page.tsx");
        let mut nodes = HashMap::new();
        nodes.insert(a.id.clone(), a);
        nodes.insert(b.id.clone(), b.clone());
        let mut map = HashMap::new();
        prefer_ambiguous_typescript_names(&nodes, &mut map);
        assert_eq!(map.get("Home"), Some(&b.id));
    }
}
