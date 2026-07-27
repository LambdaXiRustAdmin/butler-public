//! In-process query cache for `/context` responses.
//!
//! Keyed by project root + graph version + **edge percent** + prompt + mode/tool +
//! scope so edits and warehouse progress miss cleanly. A 15% partial must never
//! be served after FullEdge completes (Gem cache trap). Bounded LRU.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use super::dto::{ContextRequest, ContextResponse};

const DEFAULT_CAP: usize = 128;

/// Bounded LRU of composed context responses.
pub struct QueryCache {
    cap: usize,
    map: HashMap<u64, ContextResponse>,
    order: VecDeque<u64>,
}

impl Default for QueryCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAP)
    }
}

impl QueryCache {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get(&mut self, key: u64) -> Option<ContextResponse> {
        if let Some(v) = self.map.get(&key) {
            // touch LRU
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
                self.order.push_back(key);
            }
            return Some(v.clone());
        }
        None
    }

    pub fn insert(&mut self, key: u64, value: ContextResponse) {
        if self.map.contains_key(&key) {
            self.map.insert(key, value);
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
            }
            self.order.push_back(key);
            return;
        }
        while self.map.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        self.map.insert(key, value);
        self.order.push_back(key);
    }

}

/// Stable cache key: root + graph.version + **edge_percent** + completeness +
/// effective prompt + tool/mode + scopes.
///
/// `edge_percent` busts honest-partial answers when the warehouse climbs (15%→16%→100%).
pub fn make_query_key(
    root: &str,
    graph_version: u64,
    req: &ContextRequest,
    effective_prompt: &str,
    use_neural: bool,
    edge_percent: usize,
    edges_complete: bool,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    graph_version.hash(&mut hasher);
    // Completeness key — never freeze a 15% pack across FullEdge.
    edge_percent.min(100).hash(&mut hasher);
    edges_complete.hash(&mut hasher);
    effective_prompt.hash(&mut hasher);
    use_neural.hash(&mut hasher);
    req.mcp_tool_name.as_deref().unwrap_or("").hash(&mut hasher);
    req.mode.as_deref().unwrap_or("").hash(&mut hasher);
    req.goal.as_deref().unwrap_or("").hash(&mut hasher);
    req.target_symbol.as_deref().unwrap_or("").hash(&mut hasher);
    req.target_file.as_deref().unwrap_or("").hash(&mut hasher);
    req.target_line.unwrap_or(0).hash(&mut hasher);
    req.depth.hash(&mut hasher);
    req.max_tokens.hash(&mut hasher);
    req.full_module.hash(&mut hasher);
    req.detail.as_deref().unwrap_or("").hash(&mut hasher);
    // Soft I4 hop continuity — must not share cache with unfocused Trace of same ★.
    req.focus_symbol.as_deref().unwrap_or("").hash(&mut hasher);
    req.expand_hops.unwrap_or(0).hash(&mut hasher);
    let mut foci: Vec<&str> = req
        .focus_symbols
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    foci.sort_unstable();
    for s in foci {
        s.hash(&mut hasher);
    }
    // Soft I4 sample window — offset / mode / exclude change the pack.
    req.sample_offset.unwrap_or(0).hash(&mut hasher);
    req.sample_mode.as_deref().unwrap_or("").hash(&mut hasher);
    let mut excl: Vec<&str> = req
        .exclude_symbols
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    excl.sort_unstable();
    for s in excl {
        s.hash(&mut hasher);
    }
    // scope / ignore order-stable
    let mut scopes: Vec<&str> = req
        .scope_paths
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    scopes.sort_unstable();
    for s in scopes {
        s.hash(&mut hasher);
    }
    let mut ignores: Vec<&str> = req
        .ignore_paths
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    ignores.sort_unstable();
    for s in ignores {
        s.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_req() -> ContextRequest {
        ContextRequest {
            prompt: "TensorImpl".into(),
            root: "/projects/pytorch".into(),
            project: Some("/projects/pytorch".into()),
            depth: 2,
            max_tokens: 4000,
            compress_tests: true,
            full_module: false,
            target_file: None,
            target_line: None,
            mode: None,
            goal: Some("TraceBlastRadius".into()),
            target_symbol: Some("TensorImpl".into()),
            scope_paths: None,
            ignore_paths: None,
            focus_symbol: None,
            focus_symbols: None,
            expand_hops: None,
            sample_offset: None,
            exclude_symbols: None,
            sample_mode: None,
            detail: None,
            query: None,
            confirm_long_wait: None,
            max_results: 8,
            mcp_tool_name: None,
        }
    }

    #[test]
    fn cache_key_changes_with_edge_percent() {
        let req = bare_req();
        let a = make_query_key("/p", 1, &req, "TensorImpl", false, 15, false);
        let b = make_query_key("/p", 1, &req, "TensorImpl", false, 16, false);
        let c = make_query_key("/p", 1, &req, "TensorImpl", false, 100, true);
        assert_ne!(a, b, "15% vs 16% must bust cache");
        assert_ne!(a, c, "partial vs full must bust cache");
    }

    #[test]
    fn cache_key_changes_with_focus_symbol() {
        let mut req = bare_req();
        let a = make_query_key("/p", 1, &req, "TensorImpl", false, 100, true);
        req.focus_symbol = Some("caller_fn".into());
        let b = make_query_key("/p", 1, &req, "TensorImpl", false, 100, true);
        assert_ne!(a, b, "focus_symbol must bust cache (Soft I4)");
    }
}

/// Thread-safe wrapper for AppState.
pub type SharedQueryCache = std::sync::Arc<Mutex<QueryCache>>;

pub fn new_shared(cap: usize) -> SharedQueryCache {
    std::sync::Arc::new(Mutex::new(QueryCache::new(cap)))
}
