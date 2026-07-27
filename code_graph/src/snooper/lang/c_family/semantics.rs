//! C/C++ graph semantics that product layers (Trace, name map) must respect.

use crate::snooper::model::BlockInfo;

/// True when the C/C++ block looks like file-local `static` linkage.
pub fn looks_static_c_block(b: &BlockInfo) -> bool {
    b.source
        .lines()
        .next()
        .map(|l| {
            let t = l.trim_start();
            t.starts_with("static ") || t.starts_with("static\t")
        })
        .unwrap_or(false)
}

/// True for blocks that are C/C++ by language tag or source extension.
pub fn is_c_family_block(b: &BlockInfo) -> bool {
    let lang = b.lang.to_ascii_lowercase();
    if matches!(lang.as_str(), "c" | "cpp" | "c++" | "cxx") {
        return true;
    }
    let f = b.file.to_string_lossy().to_ascii_lowercase();
    f.ends_with(".c")
        || f.ends_with(".h")
        || f.ends_with(".cpp")
        || f.ends_with(".hpp")
        || f.ends_with(".cc")
        || f.ends_with(".cxx")
        || f.ends_with(".hh")
        || f.ends_with(".hxx")
}

/// Tree-sitter free-function prototype kind we emit from the indexer.
pub fn is_function_declaration_kind(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k == "function_declaration" || k.contains("function_declaration")
}

/// Tree-sitter function body kind.
pub fn is_function_definition_kind(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k == "function_definition" || k.ends_with("function_definition")
}

/// Header prototype ↔ definition structural edge (same symbol, decl vs def).
///
/// Must **not** appear as Trace callers/callees — LLMs read that as a call.
pub fn is_decl_def_implements_pair(a: &BlockInfo, b: &BlockInfo) -> bool {
    if a.id == b.id || a.name.is_empty() || a.name != b.name {
        return false;
    }
    if !is_c_family_block(a) && !is_c_family_block(b) {
        return false;
    }
    let a_decl = is_function_declaration_kind(&a.kind);
    let b_decl = is_function_declaration_kind(&b.kind);
    let a_def = is_function_definition_kind(&a.kind);
    let b_def = is_function_definition_kind(&b.kind);
    (a_decl && b_def) || (a_def && b_decl)
}

/// Rank boost: prefer definition body over header prototype.
pub fn impl_preference_score(b: &BlockInfo) -> i32 {
    if !is_c_family_block(b) {
        return 0;
    }
    if is_function_definition_kind(&b.kind) {
        20
    } else if is_function_declaration_kind(&b.kind) {
        5
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, Id};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(file: &str, kind: &str, name: &str, lang: &str) -> BlockInfo {
        let mut b = BlockInfo::new(
            PathBuf::from(file),
            kind,
            lang,
            1,
            3,
            0,
            10,
            format!("{kind} {name}"),
            name,
            HashSet::new(),
        );
        if kind == "function_declaration" {
            b.id = Id::new(file, kind, "decldecl1");
        }
        b
    }

    #[test]
    fn implements_pair_c_lang_tag() {
        let def = blk("src/init.c", "function_definition", "glfwInit", "c");
        let decl = blk(
            "include/GLFW/glfw3.h",
            "function_declaration",
            "glfwInit",
            "c",
        );
        assert!(is_decl_def_implements_pair(&def, &decl));
        assert!(!is_decl_def_implements_pair(
            &def,
            &blk("x.py", "function_definition", "glfwInit", "python")
        ));
    }

    #[test]
    fn prefer_definition_over_declaration() {
        let def = blk("server.c", "function_definition", "server_start", "c");
        let decl = blk("tmux.h", "function_declaration", "server_start", "c");
        assert!(impl_preference_score(&def) > impl_preference_score(&decl));
    }
}
