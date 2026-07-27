//! Language presence bitmask for interconnect gates and peer schedule.

use crate::snooper::model::CodeGraph;
use std::path::Path;

/// Cheap presence scan over inventory paths and/or node lang tags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LangPresence {
    pub rust: bool,
    pub python: bool,
    pub c_family: bool,
    pub ts_js: bool,
}

impl LangPresence {
    /// Prefer `file_hashes` keys (O(files) ≪ O(nodes)), then fill gaps from node langs.
    ///
    /// Early exit uses [`Self::all_families_seen`] — stop once every language **bit is already
    /// true**, not “stop when the project only has two langs.” Rust+Python-only still exits
    /// as soon as those two bits are set if path inventory covered them; node walk only
    /// fills missing bits (e.g. c_family via `is_c_family_block` on node kinds).
    pub fn scan(graph: &CodeGraph) -> Self {
        let mut p = Self::from_paths(graph.file_hashes.keys().map(|s| s.as_str()));
        if p.all_families_seen() {
            return p;
        }
        for b in graph.nodes.values() {
            p.absorb_lang(&b.lang);
            if !p.c_family && crate::snooper::lang::c_family::is_c_family_block(b) {
                p.c_family = true;
            }
            if p.all_families_seen() {
                break;
            }
        }
        p
    }

    /// Inventory-only presence (extensions from path list — no nodes required).
    pub fn from_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut p = Self::default();
        for path in paths {
            p.absorb_path(path);
            if p.all_families_seen() {
                break;
            }
        }
        p
    }

    pub fn absorb_path(&mut self, path: &str) {
        let f = path.replace('\\', "/").to_ascii_lowercase();
        if !self.python && f.ends_with(".py") {
            self.python = true;
        }
        if !self.rust && f.ends_with(".rs") {
            self.rust = true;
        }
        if !self.c_family
            && (f.ends_with(".c")
                || f.ends_with(".h")
                || f.ends_with(".cpp")
                || f.ends_with(".hpp")
                || f.ends_with(".cc")
                || f.ends_with(".cxx")
                || f.ends_with(".hh")
                || f.ends_with(".hxx"))
        {
            self.c_family = true;
        }
        if !self.ts_js
            && (f.ends_with(".ts")
                || f.ends_with(".tsx")
                || f.ends_with(".js")
                || f.ends_with(".jsx")
                || f.ends_with(".svelte")
                || f.ends_with(".mjs")
                || f.ends_with(".cjs"))
        {
            self.ts_js = true;
        }
    }

    pub fn absorb_lang(&mut self, lang: &str) {
        match lang.to_ascii_lowercase().as_str() {
            "rust" => self.rust = true,
            "python" => self.python = true,
            "c" | "cpp" | "c++" | "cxx" => self.c_family = true,
            "typescript" | "javascript" | "tsx" | "jsx" | "svelte" | "ts" | "js" => {
                self.ts_js = true
            }
            _ => {}
        }
    }

    /// True when every language family bit is already set — **no further scanning can help**.
    ///
    /// Not “this project uses all four languages”; a dual-stack repo that never has C/TS
    /// simply never hits this and correctly keeps scanning until path/node sources are
    /// exhausted (cheap bit absorbs only).
    fn all_families_seen(self) -> bool {
        self.rust && self.python && self.c_family && self.ts_js
    }

    /// PyO3 / pybind bridges need Python **and** a native export side.
    pub fn wants_ffi_export_map(self) -> bool {
        self.python && (self.rust || self.c_family)
    }

    /// Tauri-style IPC needs frontend + Rust.
    pub fn wants_ipc_map(self) -> bool {
        self.ts_js && self.rust
    }
}

/// True when warehouse has any TS/JS/Svelte (skip TS import LTO otherwise).
pub fn graph_has_ts_js(graph: &CodeGraph) -> bool {
    LangPresence::scan(graph).ts_js
}

pub fn graph_has_c_family(graph: &CodeGraph) -> bool {
    LangPresence::scan(graph).c_family
}

pub fn graph_has_python(graph: &CodeGraph) -> bool {
    LangPresence::scan(graph).python
}

pub fn graph_has_rust(graph: &CodeGraph) -> bool {
    LangPresence::scan(graph).rust
}

/// Extension class for dual-stack priority (parse-plan boost).
pub fn path_lang_tag(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" => Some("c_family"),
        "ts" | "tsx" | "js" | "jsx" | "svelte" | "mjs" | "cjs" => Some("ts_js"),
        _ => None,
    }
}
