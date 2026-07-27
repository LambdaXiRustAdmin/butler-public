//! File-local Python import namespace for honest CALL resolution.
//!
//! **Import-bound attribute rule** (Truth over Complexity):
//! - `import click` + `click.command()` → resolve `command` under the `click` binding.
//! - `user.save()` where `user` is not an import alias → stay silent on global bare-name
//!   (local same-file defs still link).
//!
//! No type inference. Explicit file-level syntax only.

use std::collections::HashMap;

/// One local name bound by an import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportBinding {
    /// Dotted module path as written (`click`, `django.db.models`, `.utils`).
    pub module: String,
    /// `from X import y [as z]` → export name in the module (`y`).
    /// `None` when the local name is a module alias (`import click`, `from . import utils`).
    pub export_name: Option<String>,
}

/// Local identifier → import binding (last write wins, matching Python).
pub(crate) type ImportMap = HashMap<String, ImportBinding>;

/// Parse top-level / simple import lines into a file-local alias map.
///
/// Handles:
/// - `import foo` / `import foo as bar` / `import foo.bar` / `import foo.bar as baz`
/// - `from foo import bar` / `from foo import bar as baz` / multi-name lists
/// - relative: `from . import utils`, `from .utils import clean`
///
/// Skips: `from x import *`, runaway multi-line, dynamic imports.
pub(crate) fn parse_import_map(src: &str) -> ImportMap {
    let mut map = ImportMap::new();
    let mut buf = String::new();

    for line in src.lines() {
        let t = strip_line_comment(line).trim();
        if t.is_empty() {
            continue;
        }

        if buf.is_empty() {
            if !(t.starts_with("import ") || t.starts_with("from ")) {
                continue;
            }
            buf.push_str(t);
        } else {
            buf.push(' ');
            buf.push_str(t);
        }

        // Wait for a complete-ish statement (no open paren, or closed).
        let open = buf.chars().filter(|&c| c == '(').count();
        let close = buf.chars().filter(|&c| c == ')').count();
        if open > close {
            if buf.len() > 4000 {
                buf.clear();
            }
            continue;
        }

        let stmt = buf
            .trim()
            .trim_end_matches('\\')
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        buf.clear();

        if stmt.starts_with("import ") {
            parse_import_stmt(&stmt, &mut map);
        } else if stmt.starts_with("from ") {
            parse_from_stmt(&stmt, &mut map);
        }
    }

    map
}

fn strip_line_comment(line: &str) -> &str {
    let mut in_s = false;
    let mut in_d = false;
    let mut prev = '\0';
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !in_d && prev != '\\' => in_s = !in_s,
            '"' if !in_s && prev != '\\' => in_d = !in_d,
            '#' if !in_s && !in_d => return &line[..i],
            _ => {}
        }
        prev = c;
    }
    line
}

fn parse_import_stmt(stmt: &str, map: &mut ImportMap) {
    let rest = stmt.get(7..).unwrap_or("").trim();
    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((mod_path, alias)) = split_as(part) {
            if is_dotted_ident(mod_path) && is_ident(alias) {
                map.insert(
                    alias.to_string(),
                    ImportBinding {
                        module: mod_path.to_string(),
                        export_name: None,
                    },
                );
            }
        } else if is_dotted_ident(part) {
            // `import foo.bar` binds top-level name `foo` (Python semantics).
            let top = part.split('.').next().unwrap_or(part);
            if is_ident(top) {
                map.insert(
                    top.to_string(),
                    ImportBinding {
                        module: top.to_string(),
                        export_name: None,
                    },
                );
            }
        }
    }
}

fn parse_from_stmt(stmt: &str, map: &mut ImportMap) {
    let rest = stmt.get(5..).unwrap_or("").trim();
    let Some(imp_idx) = rest.find(" import ") else {
        return;
    };
    let module = rest[..imp_idx].trim();
    if module.is_empty() || !is_relative_or_dotted_module(module) {
        return;
    }
    let mut names = rest[imp_idx + 8..].trim();
    names = names.trim_start_matches('(').trim_end_matches(')').trim();
    if names == "*" || names.is_empty() {
        return;
    }

    // `from . import utils` / `from .. import pkg` — only dots → submodule aliases.
    let pure_relative = module.chars().all(|c| c == '.');

    for part in names.split(',') {
        let part = part.trim();
        if part.is_empty() || part == "*" {
            continue;
        }
        if let Some((export, alias)) = split_as(part) {
            if !is_ident(export) || !is_ident(alias) {
                continue;
            }
            if pure_relative {
                // from . import utils as u → module alias `.utils`
                map.insert(
                    alias.to_string(),
                    ImportBinding {
                        module: format!("{module}{export}"),
                        export_name: None,
                    },
                );
            } else {
                map.insert(
                    alias.to_string(),
                    ImportBinding {
                        module: module.to_string(),
                        export_name: Some(export.to_string()),
                    },
                );
            }
        } else if is_ident(part) {
            if pure_relative {
                // from . import utils → utils is a module alias (utils.foo())
                map.insert(
                    part.to_string(),
                    ImportBinding {
                        module: format!("{module}{part}"),
                        export_name: None,
                    },
                );
            } else {
                // from pkg import name / from .models import create
                map.insert(
                    part.to_string(),
                    ImportBinding {
                        module: module.to_string(),
                        export_name: Some(part.to_string()),
                    },
                );
            }
        }
    }
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

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_dotted_ident(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_ident)
}

fn is_relative_or_dotted_module(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let stripped = s.trim_start_matches('.');
    if stripped.is_empty() {
        return s.chars().all(|c| c == '.');
    }
    is_dotted_ident(stripped)
}

/// Soft path affinity: does this block Id look like it lives under `module`?
pub(crate) fn path_affinity(id_str: &str, module: &str) -> i32 {
    let path = id_str.to_ascii_lowercase().replace('\\', "/");
    let mod_clean = module.trim_start_matches('.');
    if mod_clean.is_empty() {
        return 0;
    }
    let segs: Vec<&str> = mod_clean.split('.').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return 0;
    }
    let mut score = 0i32;
    for s in &segs {
        if path.contains(s) {
            score += 1;
        }
    }
    if let Some(last) = segs.last() {
        if path.contains(&format!("/{last}.py"))
            || path.contains(&format!("/{last}/"))
            || path.starts_with(&format!("{last}/"))
            || path.starts_with(&format!("{last}.py"))
            || path.contains(&format!("/{last}:"))
        {
            score += 2;
        }
    }
    let as_path = segs.join("/");
    if path.contains(&as_path) {
        score += 3;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_module_and_alias() {
        let m = parse_import_map("import click\nimport os.path\nimport numpy as np\n");
        assert_eq!(
            m.get("click"),
            Some(&ImportBinding {
                module: "click".into(),
                export_name: None
            })
        );
        assert_eq!(
            m.get("os"),
            Some(&ImportBinding {
                module: "os".into(),
                export_name: None
            })
        );
        assert_eq!(
            m.get("np"),
            Some(&ImportBinding {
                module: "numpy".into(),
                export_name: None
            })
        );
    }

    #[test]
    fn from_import_names_and_alias() {
        let m = parse_import_map(
            "from models import create as mk\nfrom utils import clean, helper as h\n",
        );
        assert_eq!(
            m.get("mk"),
            Some(&ImportBinding {
                module: "models".into(),
                export_name: Some("create".into())
            })
        );
        assert_eq!(
            m.get("clean"),
            Some(&ImportBinding {
                module: "utils".into(),
                export_name: Some("clean".into())
            })
        );
        assert_eq!(
            m.get("h"),
            Some(&ImportBinding {
                module: "utils".into(),
                export_name: Some("helper".into())
            })
        );
    }

    #[test]
    fn relative_from_import_submodule() {
        let m = parse_import_map("from . import utils\nfrom .models import create\n");
        assert_eq!(
            m.get("utils"),
            Some(&ImportBinding {
                module: ".utils".into(),
                export_name: None
            })
        );
        assert_eq!(
            m.get("create"),
            Some(&ImportBinding {
                module: ".models".into(),
                export_name: Some("create".into())
            })
        );
    }

    #[test]
    fn path_affinity_prefers_module_dir() {
        assert!(path_affinity("src/click/core.py:function_definition:abcd1234", "click") > 0);
        assert!(
            path_affinity("src/click/core.py:function_definition:abcd1234", "click")
                > path_affinity("src/other/core.py:function_definition:abcd1234", "click")
        );
    }
}
