//! Live Tree-sitter trees for hot files (edit-native graph update — Hop A).
//!
//! Retain `(source, Tree)` while a warehouse is hot. On edit: `InputEdit` +
//! `Parser::parse(new, Some(old_tree))` when possible. Slept roots drop trees via
//! [`clear_root`] (Complete stays on disk).

use super::lang::generic_parser::{self, VisitConfig};
use super::parser::{ParseError, ParsedFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

/// How the syntax tree was obtained for this parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// `tree.edit` + parse with old tree.
    Incremental,
    /// Full parse (no prior tree, or fallback).
    Full,
}

/// Cached live tree for one hot file.
pub struct LiveTreeEntry {
    pub source: String,
    pub tree: Tree,
}

static CACHE: OnceLock<Mutex<HashMap<String, LiveTreeEntry>>> = OnceLock::new();
static INC_PARSE: AtomicUsize = AtomicUsize::new(0);
static FULL_PARSE: AtomicUsize = AtomicUsize::new(0);

fn cache() -> &'static Mutex<HashMap<String, LiveTreeEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(root: &Path, rel: &Path) -> String {
    format!("{}::{}", root.display(), rel.display())
}

/// Drop all live trees for a warehouse root (Hop B sleep / root leave RAM).
pub fn clear_root(root: &Path) {
    let prefix = format!("{}::", root.display());
    let mut g = cache().lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|k, _| !k.starts_with(&prefix));
}

/// Drop one file's live tree (delete / failed parse).
pub fn forget_file(root: &Path, rel: &Path) {
    let mut g = cache().lock().unwrap_or_else(|e| e.into_inner());
    g.remove(&cache_key(root, rel));
}

/// Metrics: (incremental_count, full_count).
pub fn parse_counts() -> (usize, usize) {
    (
        INC_PARSE.load(Ordering::Relaxed),
        FULL_PARSE.load(Ordering::Relaxed),
    )
}

fn byte_to_point(src: &str, byte: usize) -> Point {
    let byte = byte.min(src.len());
    let mut row = 0u32;
    let mut col = 0u32;
    for (i, ch) in src.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Point {
        row: row as usize,
        column: col as usize,
    }
}

/// Single contiguous region edit from common prefix/suffix (good enough for one save).
pub fn single_region_edit(old: &str, new: &str) -> InputEdit {
    let ob = old.as_bytes();
    let nb = new.as_bytes();
    let mut start = 0usize;
    let max_start = ob.len().min(nb.len());
    while start < max_start && ob[start] == nb[start] {
        start += 1;
    }
    // UTF-8 boundary
    while start > 0 && (ob.get(start).map(|b| b & 0xc0 == 0x80).unwrap_or(false)) {
        start -= 1;
    }

    let mut old_end = ob.len();
    let mut new_end = nb.len();
    while old_end > start && new_end > start && ob[old_end - 1] == nb[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    while old_end < ob.len() && (ob[old_end] & 0xc0) == 0x80 {
        old_end += 1;
    }
    while new_end < nb.len() && (nb[new_end] & 0xc0) == 0x80 {
        new_end += 1;
    }

    InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: byte_to_point(old, start),
        old_end_position: byte_to_point(old, old_end),
        new_end_position: byte_to_point(new, new_end),
    }
}

fn set_language(parser: &mut Parser, path: &Path, source: &str) -> Result<(), ParseError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language = match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "ts" | "js" | "svelte" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" | "jsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "C" => {
            // Prefer C++ grammar for ambiguous headers when content sniffs C++.
            if super::lang::c_family::dialect_for_file(path, source)
                == super::lang::c_family::CFamilyDialect::C
            {
                tree_sitter_c::LANGUAGE.into()
            } else {
                tree_sitter_cpp::LANGUAGE.into()
            }
        }
        _ => {
            return Err(ParseError::UnknownLanguage(ext.to_string()));
        }
    };
    parser
        .set_language(&language)
        .map_err(|e| ParseError::GrammarLoad(e.to_string()))
}

/// Parse with live-tree cache when present. Records mode + changed-range count.
///
/// Blocks are extracted via the normal language `parse_file` path using the **new**
/// source; the Tree-sitter parse itself is incremental when a prior tree exists
/// (old tree passed for reuse). We then re-run language extract for definition-tier
/// blocks (visit is cheap vs full grammar reparse on large files).
pub fn parse_file_hot(
    root: &Path,
    rel: &Path,
    source: &str,
) -> Result<(ParsedFile, ParseMode, usize), ParseError> {
    let key = cache_key(root, rel);
    let prior = {
        let g = cache().lock().unwrap_or_else(|e| e.into_inner());
        g.get(&key).map(|e| (e.source.clone(), e.tree.clone()))
    };

    let t0 = Instant::now();
    let (mode, n_changed, new_tree) = if let Some((old_src, mut old_tree)) = prior {
        let mut parser = Parser::new();
        set_language(&mut parser, rel, source)?;
        let edit = single_region_edit(&old_src, source);
        old_tree.edit(&edit);
        match parser.parse(source, Some(&old_tree)) {
            Some(new_tree) => {
                let n = old_tree.changed_ranges(&new_tree).count();
                INC_PARSE.fetch_add(1, Ordering::Relaxed);
                (ParseMode::Incremental, n, Some(new_tree))
            }
            None => {
                // Fallback full
                FULL_PARSE.fetch_add(1, Ordering::Relaxed);
                let mut parser = Parser::new();
                set_language(&mut parser, rel, source)?;
                let t = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
                (ParseMode::Full, 0, Some(t))
            }
        }
    } else {
        FULL_PARSE.fetch_add(1, Ordering::Relaxed);
        let mut parser = Parser::new();
        set_language(&mut parser, rel, source)?;
        let t = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
        (ParseMode::Full, 0, Some(t))
    };

    // Definition-tier blocks from the (possibly incremental) tree — one parse only.
    let tree = new_tree.ok_or(ParseError::ParseFailed)?;
    let mut parsed = blocks_from_tree(rel.to_path_buf(), source, &tree)?;
    parsed.tree = Some(tree.clone());
    {
        let mut g = cache().lock().unwrap_or_else(|e| e.into_inner());
        g.insert(
            key,
            LiveTreeEntry {
                source: source.to_string(),
                tree,
            },
        );
    }

    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    match mode {
        ParseMode::Incremental => {
            println!(
                "🌳 parse incremental rel={} changed_ranges={} ({:.1}ms) [inc={} full={}]",
                rel.display(),
                n_changed,
                ms,
                INC_PARSE.load(Ordering::Relaxed),
                FULL_PARSE.load(Ordering::Relaxed)
            );
        }
        ParseMode::Full => {
            println!(
                "🌳 parse full rel={} ({:.1}ms) [inc={} full={}]",
                rel.display(),
                ms,
                INC_PARSE.load(Ordering::Relaxed),
                FULL_PARSE.load(Ordering::Relaxed)
            );
        }
    }

    Ok((parsed, mode, n_changed))
}

/// Seed live tree after a cold full parse (optional warm path).
pub fn seed_after_parse(root: &Path, rel: &Path, parsed: &ParsedFile) {
    if let Some(ref tree) = parsed.tree {
        let mut g = cache().lock().unwrap_or_else(|e| e.into_inner());
        g.insert(
            cache_key(root, rel),
            LiveTreeEntry {
                source: parsed.source.clone(),
                tree: tree.clone(),
            },
        );
    }
}

/// Definition-tier blocks from an existing tree (no second Tree-sitter parse).
fn blocks_from_tree(
    path: PathBuf,
    source: &str,
    tree: &Tree,
) -> Result<ParsedFile, ParseError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let root = tree.root_node();
    let mut blocks = Vec::new();
    let fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| format!("{s}_default"))
        .unwrap_or_else(|| "unknown".to_string());

    match ext {
        "rs" => {
            let config = VisitConfig {
                interesting_kinds: &[
                    "function_item",
                    "struct_item",
                    "enum_item",
                    "union_item",
                    "trait_item",
                    "impl_item",
                    "mod_item",
                    "type_item",
                    "const_item",
                    "static_item",
                ],
                lang: "rust",
                extract_name: name_field_or_none,
                get_start: generic_parser::default_get_start,
                extract_externals: generic_parser::no_external_crates,
            };
            generic_parser::visit_node(root, path.clone(), source, None, &mut blocks, config, "unknown");
        }
        "py" => {
            let config = VisitConfig {
                interesting_kinds: &[
                    "function_definition",
                    "class_definition",
                    "async_function_definition",
                ],
                lang: "python",
                extract_name: name_field_or_none,
                get_start: generic_parser::default_get_start,
                extract_externals: generic_parser::no_external_crates,
            };
            generic_parser::visit_node(root, path.clone(), source, None, &mut blocks, config, "unknown");
        }
        "ts" | "tsx" | "js" | "jsx" | "svelte" => {
            let config = VisitConfig {
                interesting_kinds: &[
                    "class_declaration",
                    "interface_declaration",
                    "function_declaration",
                    "method_definition",
                    "arrow_function",
                    "variable_declarator",
                ],
                lang: "typescript",
                extract_name: name_field_or_none,
                get_start: generic_parser::default_get_start,
                extract_externals: generic_parser::no_external_crates,
            };
            generic_parser::visit_node(
                root,
                path.clone(),
                source,
                None,
                &mut blocks,
                config,
                &fallback,
            );
        }
        "go" => {
            let config = VisitConfig {
                interesting_kinds: &[
                    "function_declaration",
                    "method_declaration",
                    "type_spec",
                ],
                lang: "go",
                extract_name: name_field_or_none,
                get_start: generic_parser::default_get_start,
                extract_externals: generic_parser::no_external_crates,
            };
            generic_parser::visit_node(
                root,
                path.clone(),
                source,
                None,
                &mut blocks,
                config,
                &fallback,
            );
        }
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "C" => {
            // Fall back to full language parse for C-family (dialect-sensitive).
            return super::parser::parse_file(path, source);
        }
        _ => return Err(ParseError::UnknownLanguage(ext.to_string())),
    }

    Ok(ParsedFile {
        path,
        source: source.to_string(),
        blocks,
        tree: None, // caller sets tree
    })
}

fn name_field_or_none(node: &Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_region_edit_detects_middle_insert() {
        let old = "fn foo() {\n  bar();\n}\n";
        let new = "fn foo() {\n  // x\n  bar();\n}\n";
        let e = single_region_edit(old, new);
        assert!(e.start_byte > 0);
        assert!(e.new_end_byte > e.old_end_byte || e.new_end_byte != e.old_end_byte);
    }

    #[test]
    fn parse_hot_second_call_is_incremental() {
        let root = std::env::temp_dir().join(format!("butler_live_tree_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let rel = PathBuf::from("x.ts");
        let abs = root.join(&rel);
        std::fs::write(&abs, "export function a() { return 1 }\n").unwrap();
        let s1 = std::fs::read_to_string(&abs).unwrap();
        let (p1, m1, _) = parse_file_hot(&root, &rel, &s1).unwrap();
        assert!(matches!(m1, ParseMode::Full));
        assert!(p1.tree.is_some());

        let s2 = "export function a() {\n  // edit\n  return 1\n}\n";
        let (_p2, m2, _nchg) = parse_file_hot(&root, &rel, s2).unwrap();
        assert!(
            matches!(m2, ParseMode::Incremental),
            "second parse should be incremental"
        );
        clear_root(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_root_forces_full_then_second_edit_incremental() {
        // Sleep drops live trees; first touch full-parses; subsequent edit while hot is incremental.
        let root = std::env::temp_dir().join(format!("butler_live_tree_sleep_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let rel = PathBuf::from("y.ts");
        let s1 = "export function b() { return 2 }\n";
        let (_p1, m1, _) = parse_file_hot(&root, &rel, s1).unwrap();
        assert!(matches!(m1, ParseMode::Full));
        clear_root(&root); // sleep
        let s2 = "export function b() {\n  return 2\n}\n";
        let (_p2, m2, _) = parse_file_hot(&root, &rel, s2).unwrap();
        assert!(
            matches!(m2, ParseMode::Full),
            "first edit after clear_root must full-parse"
        );
        let s3 = "export function b() {\n  // hot\n  return 2\n}\n";
        let (_p3, m3, _) = parse_file_hot(&root, &rel, s3).unwrap();
        assert!(
            matches!(m3, ParseMode::Incremental),
            "second edit while hot must be incremental"
        );
        clear_root(&root);
        let _ = std::fs::remove_dir_all(&root);
    }
}
