//! Per-file TypeScript/JavaScript **export table**.
//!
//! Line-based, same drawer DNA as [`super::imports`]. Consumed by the LTO
//! re-export walk in [`super::imports::resolve_exported_name`].
//!
//! Tracks named exports and star re-exports **separately** so Cut 2 can enforce
//! "named wins over star" without merging prematurely.
//!
//! Shapes (single-line / simple join only for Cut 1):
//! - `export const|function|class|async function X`
//! - `export { X, Y as Z }` (local)
//! - `export { X, Y as Z } from './mod'`
//! - `export * from './mod'`
//!
//! Multi-line heavy barrels: intentionally incomplete → silence until Tree-sitter.

use std::collections::HashMap;

/// Where an exported binding comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTarget {
    /// Defined or re-exported locally in this file (`export const X` / `export { X }`).
    Local,
    /// `export { Orig as Name } from './mod'` → module path + name in that module.
    /// Map key is the **exported** name (`Name`); second field is `Orig` (often same).
    Named(String, String),
}

/// File-local export surface for the re-export walk (Cut 2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportTable {
    /// Exported name → origin (local def or named re-export).
    pub named_exports: HashMap<String, ExportTarget>,
    /// Ordered `export * from '…'` module paths (first-to-last as written).
    pub star_exports: Vec<String>,
}

/// Parse a source file into an [`ExportTable`]. Best-effort line parse; no inventing.
pub fn parse_export_table(src: &str) -> ExportTable {
    let mut table = ExportTable::default();
    let mut buf = String::new();

    for line in src.lines() {
        let t = strip_line_comment(line).trim();
        if t.is_empty() {
            continue;
        }

        if buf.is_empty() {
            if !t.starts_with("export ") {
                continue;
            }
            buf.push_str(t);
        } else {
            buf.push(' ');
            buf.push_str(t);
        }

        // Complete-ish: no open brace, or braces balanced; no open paren for `export function (`
        let open_b = buf.chars().filter(|&c| c == '{').count();
        let close_b = buf.chars().filter(|&c| c == '}').count();
        let open_p = buf.chars().filter(|&c| c == '(').count();
        let close_p = buf.chars().filter(|&c| c == ')').count();
        if open_b > close_b || open_p > close_p {
            if buf.len() > 4000 {
                buf.clear();
            }
            continue;
        }

        let stmt = buf
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        buf.clear();
        ingest_export_stmt(&stmt, &mut table);
    }

    // Trailing incomplete buffer: drop (silence).
    table
}

fn ingest_export_stmt(stmt: &str, table: &mut ExportTable) {
    let s = stmt.trim().trim_end_matches(';').trim();
    if !s.starts_with("export ") {
        return;
    }
    // `export type` / `export interface` — treat as local named when simple.
    let rest = s.get(7..).unwrap_or("").trim();
    let rest = rest
        .strip_prefix("default ")
        .map(|r| r.trim())
        .unwrap_or(rest);

    // --- star re-export: export * from '…' / export * as ns from '…' (path only; ns later)
    if let Some(after_star) = rest.strip_prefix('*') {
        let after_star = after_star.trim();
        // `from './x'` | `as ns from './x'` — not always spaced as ` from `.
        let from_idx = after_star
            .find(" from ")
            .or_else(|| after_star.starts_with("from ").then_some(0));
        if let Some(from_idx) = from_idx {
            let mut mod_part = if from_idx == 0 && after_star.starts_with("from ") {
                after_star["from ".len()..].trim()
            } else {
                after_star[from_idx + " from ".len()..].trim()
            };
            if let Some(i) = mod_part.find("//") {
                mod_part = mod_part[..i].trim();
            }
            let path = strip_quotes(mod_part);
            if !path.is_empty() {
                table.star_exports.push(path);
            }
        }
        return;
    }

    // --- export { … } [from '…']
    if rest.starts_with('{') {
        if let Some(close) = rest.find('}') {
            let inner = rest.get(1..close).unwrap_or("").trim();
            let after = rest.get(close + 1..).unwrap_or("").trim();
            if let Some(from_idx) = after.find("from ") {
                let mut mod_part = after[from_idx + 5..].trim();
                if let Some(i) = mod_part.find("//") {
                    mod_part = mod_part[..i].trim();
                }
                let module = strip_quotes(mod_part);
                if module.is_empty() {
                    return;
                }
                for (export_name, orig) in parse_export_clause(inner) {
                    table
                        .named_exports
                        .insert(export_name, ExportTarget::Named(module.clone(), orig));
                }
            } else {
                // local: export { helper } / export { a as b }
                for (export_name, _orig) in parse_export_clause(inner) {
                    table
                        .named_exports
                        .insert(export_name, ExportTarget::Local);
                }
            }
        }
        return;
    }

    // --- export const|let|var|function|class|async function|interface|type Name
    if let Some(name) = parse_local_declaration_name(rest) {
        table.named_exports.insert(name, ExportTarget::Local);
    }
}

/// `a, b as c, type D` → (exported_name, original_name) pairs. Skips `type` keyword tokens.
fn parse_export_clause(inner: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // strip leading `type ` (TS type-only export)
        let part = part.strip_prefix("type ").unwrap_or(part).trim();
        if part.is_empty() || part == "type" {
            continue;
        }
        if let Some((orig, alias)) = split_as(part) {
            if is_ident(orig) && is_ident(alias) {
                out.push((alias.to_string(), orig.to_string()));
            }
        } else if is_ident(part) {
            out.push((part.to_string(), part.to_string()));
        }
    }
    out
}

fn parse_local_declaration_name(rest: &str) -> Option<String> {
    let rest = rest.trim();
    // optional `async `
    let rest = rest.strip_prefix("async ").unwrap_or(rest).trim();

    for kw in ["function ", "class ", "const ", "let ", "var ", "interface ", "type ", "enum "] {
        if let Some(after) = rest.strip_prefix(kw) {
            let after = after.trim();
            // function Name(  /  class Name  /  const Name =
            let name = after
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
                .next()
                .unwrap_or("");
            if is_ident(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn split_as(part: &str) -> Option<(&str, &str)> {
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

fn strip_line_comment(line: &str) -> &str {
    if let Some(i) = find_unquoted_line_comment(line) {
        &line[..i]
    } else {
        line
    }
}

fn find_unquoted_line_comment(line: &str) -> Option<usize> {
    let mut in_s = false;
    let mut in_d = false;
    let mut prev = '\0';
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !in_d && prev != '\\' => in_s = !in_s,
            '"' if !in_s && prev != '\\' => in_d = !in_d,
            '/' if !in_s && !in_d && prev == '/' => return Some(i.saturating_sub(1)),
            _ => {}
        }
        prev = c;
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim().trim_end_matches(';').trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s.get(1..s.len() - 1).unwrap_or("").to_string()
    } else {
        s.to_string()
    }
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

    /// Test A: standard barrel — only star, no named.
    #[test]
    fn test_a_standard_barrel_star_only() {
        // Conceptual: Button.tsx has the def; index only re-exports.
        let index = "export * from './Button'\n";
        let table = parse_export_table(index);
        assert!(
            table.named_exports.is_empty(),
            "barrel must not invent named exports: {:?}",
            table.named_exports
        );
        assert_eq!(table.star_exports, vec!["./Button".to_string()]);
    }

    /// Test B: named re-export coexists with star; named recorded for walk preference.
    #[test]
    fn test_b_named_over_star_recorded_separately() {
        let index = r#"
export * from './Text'
export { Button } from './Button'
"#;
        let table = parse_export_table(index);
        assert_eq!(
            table.named_exports.get("Button"),
            Some(&ExportTarget::Named("./Button".into(), "Button".into()))
        );
        assert_eq!(table.star_exports, vec!["./Text".to_string()]);
        // Named must not be folded into star list.
        assert!(!table.star_exports.iter().any(|p| p.contains("Button")));
    }

    /// Test C: local re-export clause.
    #[test]
    fn test_c_local_export_clause() {
        let utils = r#"
const helper = () => {};
export { helper };
"#;
        let table = parse_export_table(utils);
        assert_eq!(
            table.named_exports.get("helper"),
            Some(&ExportTarget::Local)
        );
        assert!(table.star_exports.is_empty());
    }

    #[test]
    fn local_function_class_const() {
        let src = r#"
export function Button() {}
export class Card {}
export const Icon = () => null
export async function load() {}
"#;
        let table = parse_export_table(src);
        assert_eq!(table.named_exports.get("Button"), Some(&ExportTarget::Local));
        assert_eq!(table.named_exports.get("Card"), Some(&ExportTarget::Local));
        assert_eq!(table.named_exports.get("Icon"), Some(&ExportTarget::Local));
        assert_eq!(table.named_exports.get("load"), Some(&ExportTarget::Local));
    }

    #[test]
    fn named_reexport_with_alias() {
        let src = "export { Button as Btn } from './Button'\n";
        let table = parse_export_table(src);
        assert_eq!(
            table.named_exports.get("Btn"),
            Some(&ExportTarget::Named("./Button".into(), "Button".into()))
        );
        assert!(!table.named_exports.contains_key("Button"));
    }

    #[test]
    fn ignores_non_export_and_bare_side_effects() {
        let src = r#"
import { x } from './y'
export {} from './empty'
const z = 1
"#;
        let table = parse_export_table(src);
        // empty clause → no names
        assert!(table.named_exports.is_empty());
        assert!(table.star_exports.is_empty());
    }

    #[test]
    fn star_as_namespace_still_records_module() {
        // Cut 1: record path only; namespace binding is a later concern.
        let src = "export * as UI from './ui'\n";
        let table = parse_export_table(src);
        assert_eq!(table.star_exports, vec!["./ui".to_string()]);
    }
}
