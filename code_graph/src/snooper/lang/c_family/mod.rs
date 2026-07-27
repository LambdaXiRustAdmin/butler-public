//! Shared C / C++ family helpers.
//!
//! **Dialect detection** is extension + content sniff only — no project config.
//! Parse and edge collection must use the same dialect so the Tree-sitter
//! language object matches the AST (C query on C tree, C++ query on C++ tree).

pub mod ast_shared;
pub mod ffi;
pub mod semantics;

use std::path::Path;

pub use ffi::{collect_pybind_mdef_exports, parse_mdef_bindings};
pub use semantics::{
    impl_preference_score, is_c_family_block, is_decl_def_implements_pair,
    is_function_declaration_kind, is_function_definition_kind, looks_static_c_block,
};

/// Which Tree-sitter grammar + call query to use for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CFamilyDialect {
    /// `tree-sitter-c` — `.c` and C-shaped `.h`
    C,
    /// `tree-sitter-cpp` — `.cpp`/`.hpp`/… and C++-shaped `.h`
    Cpp,
}

impl CFamilyDialect {
    pub fn lang_tag(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "cpp",
        }
    }
}

/// Choose dialect without external guidance.
///
/// | Path | Rule |
/// |------|------|
/// | `.c` | always C |
/// | `.cpp` `.cc` `.cxx` `.C` `.hpp` `.hh` `.hxx` | always C++ |
/// | `.h` | C++ if source sniffs as C++; else C |
pub fn dialect_for_file(path: &Path, source: &str) -> CFamilyDialect {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "c" => CFamilyDialect::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => CFamilyDialect::Cpp,
        // Capital `.C` often means C++ on Unix
        _ if path
            .extension()
            .and_then(|e| e.to_str())
            == Some("C") =>
        {
            CFamilyDialect::Cpp
        }
        "h" => {
            if looks_like_cpp_source(source) {
                CFamilyDialect::Cpp
            } else {
                CFamilyDialect::C
            }
        }
        // Unknown but routed here → prefer C++ (broader grammar) only if sniffs; else C
        _ => {
            if looks_like_cpp_source(source) {
                CFamilyDialect::Cpp
            } else {
                CFamilyDialect::C
            }
        }
    }
}

/// Cheap content sniff for ambiguous `.h` (and fallbacks). No full parse.
pub fn looks_like_cpp_source(source: &str) -> bool {
    // Scan a prefix + a few keyword hits — enough for headers without false C++ on pure C.
    let sample = if source.len() > 64_000 {
        &source[..64_000]
    } else {
        source
    };
    // Strong C++-only tokens / constructs
    const MARKERS: &[&str] = &[
        "namespace ",
        "namespace{",
        "template<",
        "template <",
        "typename ",
        "constexpr ",
        "noexcept",
        "public:",
        "private:",
        "protected:",
        "using namespace",
        "operator",
        "class ",
        "std::",
        "nullptr",
        "override",
        "final ",
        "concept ",
        "requires ",
        "co_await",
        "co_yield",
        "co_return",
        "decltype",
        "static_assert",
        "dynamic_cast",
        "reinterpret_cast",
        "static_cast",
        "const_cast",
        "::",
    ];
    for m in MARKERS {
        if sample.contains(m) {
            // `::` alone is rare in pure C; still exclude simple `http://` false path via `://`
            if *m == "::" && sample.contains("://") && !sample.contains("std::") {
                // might be only URLs — require another marker
                continue;
            }
            return true;
        }
    }
    false
}

/// Extensions that go through the C or C++ modules.
pub fn is_c_family_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx"
    ) || ext == "C"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn dialect_by_extension() {
        assert_eq!(
            dialect_for_file(Path::new("a.c"), "int x;"),
            CFamilyDialect::C
        );
        assert_eq!(
            dialect_for_file(Path::new("a.cpp"), "int x;"),
            CFamilyDialect::Cpp
        );
        assert_eq!(
            dialect_for_file(Path::new("a.hpp"), "int x;"),
            CFamilyDialect::Cpp
        );
    }

    #[test]
    fn h_defaults_to_c_without_cpp_markers() {
        let src = "int glfwInit(void);\nstruct Foo { int x; };\n";
        assert_eq!(
            dialect_for_file(Path::new("include/glfw3.h"), src),
            CFamilyDialect::C
        );
    }

    #[test]
    fn h_sniffs_cpp_on_class_namespace() {
        let src = "namespace N { class Foo { public: void bar(); }; }\n";
        assert_eq!(
            dialect_for_file(Path::new("foo.h"), src),
            CFamilyDialect::Cpp
        );
    }
}
