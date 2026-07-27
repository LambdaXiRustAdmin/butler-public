//! Detect bridge signals → peer-lang schedule (Track P.3).
//!
//! Does **not** invent edges. Only reports which peer languages the interconnect
//! layer wants parsed so Export/Ipc maps can run.

use super::presence::{path_lang_tag, LangPresence};
use crate::snooper::model::CodeGraph;
use crate::snooper::project_paths::ProjectPaths;
use std::path::Path;

/// Peer languages still needed for interconnect honesty.
#[derive(Debug, Clone, Default)]
pub struct PeerSchedule {
    /// Languages present in inventory/nodes.
    pub present: LangPresence,
    /// Languages we should pull into the parse/edge plan for bridges.
    pub need: LangPresence,
    /// Human-readable signal reasons (capped).
    pub reasons: Vec<String>,
}

impl PeerSchedule {
    pub fn has_needs(&self) -> bool {
        self.need.python || self.need.rust || self.need.c_family || self.need.ts_js
    }

    pub fn log_if_needed(&self) {
        if !self.has_needs() {
            return;
        }
        let mut parts = Vec::new();
        if self.need.python {
            parts.push("python");
        }
        if self.need.rust {
            parts.push("rust");
        }
        if self.need.c_family {
            parts.push("c_family");
        }
        if self.need.ts_js {
            parts.push("ts_js");
        }
        println!(
            "📡 Interconnect peer schedule: need [{}] — {}",
            parts.join(", "),
            if self.reasons.is_empty() {
                "dual-stack inventory".to_string()
            } else {
                self.reasons.join("; ")
            }
        );
    }
}

/// Infer peer-lang needs from inventory co-presence + light export/IPC signals.
pub fn detect_peer_schedule(graph: &CodeGraph, project_root: Option<&Path>) -> PeerSchedule {
    let present = LangPresence::scan(graph);
    let inv = LangPresence::from_paths(graph.file_hashes.keys().map(|s| s.as_str()));
    let mut need = LangPresence::default();
    let mut reasons: Vec<String> = Vec::new();

    // Dual-stack inventory but one side never parsed into nodes yet (progressive waves).
    if inv.python && inv.rust {
        if present.rust && !present.python {
            need.python = true;
            reasons.push("inventory has .py + .rs; python nodes missing".into());
        }
        if present.python && !present.rust {
            need.rust = true;
            reasons.push("inventory has .py + .rs; rust nodes missing".into());
        }
    }
    if inv.python && inv.c_family {
        if present.c_family && !present.python {
            need.python = true;
            push_reason(&mut reasons, "inventory has python + c_family; python nodes missing");
        }
        if present.python && !present.c_family {
            need.c_family = true;
            push_reason(&mut reasons, "inventory has python + c_family; c_family nodes missing");
        }
    }
    if inv.ts_js && inv.rust {
        if present.rust && !present.ts_js {
            need.ts_js = true;
            push_reason(&mut reasons, "inventory has ts/js + rust; frontend nodes missing");
        }
        if present.ts_js && !present.rust {
            need.rust = true;
            push_reason(&mut reasons, "inventory has ts/js + rust; rust nodes missing");
        }
    }

    // Signal: #[pyfunction] on rust side without python presence → want python peer.
    if present.rust && !present.python {
        if rust_has_pyfunction_signal(graph, project_root) {
            need.python = true;
            push_reason(&mut reasons, "#[pyfunction] export signal without python nodes");
        }
    }
    // Signal: m.def without python.
    if present.c_family && !present.python {
        if c_has_pybind_signal(graph, project_root) {
            need.python = true;
            push_reason(&mut reasons, "pybind m.def signal without python nodes");
        }
    }

    PeerSchedule {
        present,
        need,
        reasons,
    }
}

fn push_reason(reasons: &mut Vec<String>, msg: &str) {
    // Cap keeps peer-schedule logs short (health / dogfood); not a hard product limit.
    if reasons.len() < 8 && !reasons.iter().any(|r| r == msg) {
        reasons.push(msg.into());
    }
}

/// Cheap whole-file signal for peer schedule — not Export collect precision.
///
/// **Double-check (block then file):** progressive/slim graph may strip `BlockInfo.source`
/// while the on-disk file still holds `#[pyfunction]` / pybind markers. Block hit = fast path;
/// empty or miss → one disk read **per unique file** (cached for this call).
fn rust_has_pyfunction_signal(graph: &CodeGraph, project_root: Option<&Path>) -> bool {
    let pp = project_root.map(ProjectPaths::new);
    // Avoid O(functions) re-reads of the same .rs (many function_item share one crate file).
    let mut file_cache: std::collections::HashMap<std::path::PathBuf, Option<String>> =
        std::collections::HashMap::new();
    for b in graph.nodes.values() {
        if b.lang != "rust" || b.kind != "function_item" {
            continue;
        }
        if !b.source.is_empty() && b.source.to_ascii_lowercase().contains("pyfunction") {
            return true;
        }
        if let Some(ref paths) = pp {
            let abs = paths.to_abs(&b.file);
            let text = file_cache
                .entry(abs.clone())
                .or_insert_with(|| std::fs::read_to_string(&abs).ok());
            if let Some(s) = text.as_ref() {
                if s.to_ascii_lowercase().contains("pyfunction") {
                    return true;
                }
            }
        }
    }
    false
}

/// Same slim-source / per-file cache contract as [`rust_has_pyfunction_signal`].
fn c_has_pybind_signal(graph: &CodeGraph, project_root: Option<&Path>) -> bool {
    let pp = project_root.map(ProjectPaths::new);
    let mut file_cache: std::collections::HashMap<std::path::PathBuf, Option<String>> =
        std::collections::HashMap::new();
    for b in graph.nodes.values() {
        if !crate::snooper::lang::c_family::is_c_family_block(b) {
            continue;
        }
        if !b.source.is_empty()
            && (b.source.contains("m.def") || b.source.contains("PYBIND11_MODULE"))
        {
            return true;
        }
        if let Some(ref paths) = pp {
            let abs = paths.to_abs(&b.file);
            let text = file_cache
                .entry(abs.clone())
                .or_insert_with(|| std::fs::read_to_string(&abs).ok());
            if let Some(s) = text.as_ref() {
                if s.contains("m.def") || s.contains("PYBIND11_MODULE") {
                    return true;
                }
            }
        }
    }
    false
}

/// When dual-stack inventory exists, boost the secondary lang into early parse waves.
///
/// Returns a lower priority (earlier) for paths of under-represented peer langs.
/// `None` = no boost (use normal monorepo priority).
pub fn dual_stack_parse_boost(path: &Path, inventory: LangPresence) -> Option<u8> {
    let tag = path_lang_tag(path)?;
    // Dual native↔python: pull .py early when both exist in inventory.
    if inventory.python && (inventory.rust || inventory.c_family) {
        if tag == "python" {
            return Some(0);
        }
        if tag == "rust" || tag == "c_family" {
            return Some(0);
        }
    }
    // Dual ts+rust: pull both early.
    if inventory.ts_js && inventory.rust {
        if tag == "ts_js" || tag == "rust" {
            return Some(0);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snooper::model::{BlockInfo, CodeGraph};
    use std::collections::HashSet;

    fn blk(name: &str, lang: &str, file: &str, source: &str) -> BlockInfo {
        BlockInfo::new(
            file,
            if lang == "rust" {
                "function_item"
            } else {
                "function_definition"
            },
            lang,
            1,
            2,
            0,
            source.len(),
            source.to_string(),
            name,
            HashSet::new(),
        )
    }

    #[test]
    fn pyfunction_signal_schedules_python_peer() {
        let mut g = CodeGraph::new();
        g.file_hashes.insert("src/lib.rs".into(), 1);
        let r = blk(
            "search",
            "rust",
            "src/lib.rs",
            "#[pyfunction]\nfn search() {}\n",
        );
        g.nodes.insert(r.id.clone(), r);
        let sch = detect_peer_schedule(&g, None);
        assert!(
            sch.need.python,
            "pyfunction signal should need python peer: {:?}",
            sch.reasons
        );
    }

    #[test]
    fn dual_stack_boost_pulls_py_early() {
        let inv = LangPresence {
            rust: true,
            python: true,
            ..Default::default()
        };
        assert_eq!(
            dual_stack_parse_boost(Path::new("word_count/__init__.py"), inv),
            Some(0)
        );
        assert_eq!(
            dual_stack_parse_boost(Path::new("README.md"), inv),
            None
        );
    }

    #[test]
    fn no_schedule_when_both_sides_present() {
        let mut g = CodeGraph::new();
        g.file_hashes.insert("a.rs".into(), 1);
        g.file_hashes.insert("b.py".into(), 2);
        let r = blk("a", "rust", "a.rs", "fn a() {}");
        let p = blk("b", "python", "b.py", "def b():\n  pass\n");
        g.nodes.insert(r.id.clone(), r);
        g.nodes.insert(p.id.clone(), p);
        let sch = detect_peer_schedule(&g, None);
        assert!(!sch.need.python && !sch.need.rust);
    }
}
