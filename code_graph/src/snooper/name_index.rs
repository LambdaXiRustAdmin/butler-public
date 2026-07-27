//! Name index + multi-file collisions + same-name peer callers (P2a peel from model.rs).
//!
//! Split `impl CodeGraph` methods for the product name map. Types (`NameLocation`,
//! `NameIndexAudit`) stay in [`super::model`]. Zero intentional behavior change.

use std::collections::HashMap;

use super::model::{BlockInfo, CodeGraph, Id, NameIndexAudit, NameLocation};
use super::normalize_path;

/// Names that flood monorepo `name_index` but teach the mill nothing (or lie by volume).
/// Used by [`CodeGraph::multi_file_name_collisions`] and kept in sync with the probe.
fn is_collision_mine_junk(name: &str) -> bool {
    if super::query_tokens::is_junk_symbol_name(name) {
        return true;
    }
    matches!(
        name,
        // Parser / placeholder shells (wasmtime `unknown` alone is 1k+ files).
        "unknown"
            | "Unknown"
            | "UNKNOWN"
            // Module / test packages, not callable seeds
            | "tests"
            | "test"
            | "Test"
            // Result/Option constructors — not useful CALL collision mines
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            // Language / JSON literals
            | "true"
            | "false"
            | "null"
            | "nil"
            | "undefined"
            | "void"
            // Ultra-generic type tokens that dominate polyglot warehouses
            | "int"
            | "str"
            | "bool"
            | "float"
            | "string"
            | "bytes"
            | "object"
            | "any"
    )
}


impl CodeGraph {
    /// Rebuild exact-name → locations **and** file → node-id indexes (one O(n) pass).
    pub fn rebuild_name_index(&mut self) {
        let mut idx: HashMap<String, Vec<NameLocation>> = HashMap::new();
        let mut by_file: HashMap<String, Vec<Id>> =
            HashMap::with_capacity(self.file_hashes.len().max(64));
        for b in self.nodes.values() {
            if !b.name.is_empty() {
                idx.entry(b.name.clone()).or_default().push(NameLocation {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    file: b.file.clone(),
                    start_line: b.start_line,
                    end_line: b.end_line,
                    kind: b.kind.clone(),
                    lang: b.lang.clone(),
                });
            }
            let fkey = normalize_path(&b.file.to_string_lossy());
            by_file.entry(fkey).or_default().push(b.id.clone());
        }
        for locs in idx.values_mut() {
            locs.sort_by(|a, b| {
                a.file
                    .cmp(&b.file)
                    .then(a.start_line.cmp(&b.start_line))
                    .then_with(|| a.id.as_str().cmp(b.id.as_str()))
            });
        }
        self.name_index = idx;
        self.name_index_nodes_len = self.nodes.len();
        self.file_node_index = by_file;
        self.file_node_index_nodes_len = self.nodes.len();
    }

    /// O(1): file→nodes index ready for file-local scope collect.
    #[inline]
    pub fn file_node_index_is_warm(&self) -> bool {
        !self.file_node_index.is_empty()
            && self.file_node_index_nodes_len == self.nodes.len()
            && !self.nodes.is_empty()
    }

    /// Build only path→ids (when name_index was loaded from bin without a full rebuild).
    pub fn rebuild_file_node_index_only(&mut self) {
        let mut by_file: HashMap<String, Vec<Id>> =
            HashMap::with_capacity(self.file_hashes.len().max(64));
        for b in self.nodes.values() {
            let fkey = normalize_path(&b.file.to_string_lossy());
            by_file.entry(fkey).or_default().push(b.id.clone());
        }
        self.file_node_index = by_file;
        self.file_node_index_nodes_len = self.nodes.len();
    }

    /// Exact name lookup (rg-shaped hit list). Empty if unknown or index empty.
    pub fn locations_for_name(&self, name: &str) -> &[NameLocation] {
        self.name_index
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Multi-file same-name collisions from `name_index` (repo-agnostic spectacular seed mine).
    ///
    /// Prefers function/method-like kinds; ranks by unique file count desc then name.
    /// Drops junk shells (`unknown`, single-letter, parser tokens) so `/collisions`
    /// and tier-2 mills do not burn budget on mega-homonym noise.
    /// `min_files`: distinct file paths with this name (default-style: ≥2).
    /// `max`: hard cap on returned names.
    pub fn multi_file_name_collisions(
        &self,
        min_files: usize,
        max: usize,
        min_name_len: usize,
    ) -> Vec<(String, usize, usize)> {
        // (name, n_locs, n_files)
        let min_files = min_files.max(2);
        let mut out: Vec<(String, usize, usize)> = Vec::new();
        for (name, locs) in &self.name_index {
            if name.len() < min_name_len || name.is_empty() {
                continue;
            }
            // Skip pure noise tokens
            if name.chars().all(|c| !c.is_alphanumeric()) {
                continue;
            }
            if is_collision_mine_junk(name) {
                continue;
            }
            let mut has_def = false;
            let mut files: std::collections::HashSet<String> =
                std::collections::HashSet::with_capacity(locs.len().min(64));
            for loc in locs {
                let k = loc.kind.to_ascii_lowercase();
                // Prefer real defs; still count multi-file type/impl for H-class pins
                if k.contains("function")
                    || k.contains("method")
                    || k.contains("class")
                    || k.contains("struct")
                    || k.contains("impl")
                    || k.contains("type")
                    || k.contains("trait")
                    || k.contains("enum")
                    || k.contains("const")
                    || k.contains("static")
                    || k.contains("mod")
                {
                    has_def = true;
                }
                // Skip pure call-expression shells as the only evidence
                if k.contains("call_expression") || k == "call" {
                    continue;
                }
                files.insert(loc.file.to_string_lossy().replace('\\', "/"));
            }
            if !has_def || files.len() < min_files {
                continue;
            }
            out.push((name.clone(), locs.len(), files.len()));
        }
        out.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        if out.len() > max {
            out.truncate(max);
        }
        out
    }

    /// O(1): index empty while nodes exist, or node set grew/shrank since last rebuild stamp.
    #[inline]
    pub fn name_index_is_stale(&self) -> bool {
        if self.nodes.is_empty() {
            return !self.name_index.is_empty();
        }
        if self.name_index.is_empty() {
            return true;
        }
        // 0 stamp after load-without-rebuild path: treat non-empty index as needing verify.
        // Callers should prefer [`ensure_name_index`] which stamps after rebuild/audit.
        self.name_index_nodes_len != self.nodes.len()
    }

    /// Ensure index exists and matches current `nodes.len()` (rebuild on growth / cold).
    pub fn ensure_name_index(&mut self) {
        if self.nodes.is_empty() {
            if !self.name_index.is_empty() {
                self.name_index.clear();
                self.name_index_nodes_len = 0;
            }
            self.file_node_index.clear();
            self.file_node_index_nodes_len = 0;
            return;
        }
        if self.name_index_is_stale() {
            let before_keys = self.name_index.len();
            let before_nodes = self.name_index_nodes_len;
            self.rebuild_name_index();
            if before_keys > 0 && before_nodes != self.nodes.len() {
                println!(
                    "📇 name_index rebuild (stale stamp): nodes {}→{}, keys {}→{}",
                    before_nodes,
                    self.nodes.len(),
                    before_keys,
                    self.name_index.len()
                );
            }
        } else if !self.file_node_index_is_warm() {
            self.rebuild_file_node_index_only();
        }
    }

    /// **Index enforcer:** resolve exact-name blocks via `name_index` (O(hits)).
    ///
    /// - Warm, stamped index + zero hits → real miss (empty). **No mountain walk.**
    /// - Cold or stale stamp (empty map / `name_index_nodes_len != nodes.len()`) → linear
    ///   fallback so growth after load cannot false-miss hubs (click `Group` class).
    /// - Warm miss while stamp says fresh: trust index (callers with write should
    ///   [`ensure_name_index`] after node mutations).
    pub fn blocks_for_name(&self, name: &str) -> Vec<&BlockInfo> {
        if name.is_empty() {
            return vec![];
        }
        let locs = self.locations_for_name(name);
        if !locs.is_empty() {
            return locs
                .iter()
                .filter_map(|loc| self.nodes.get(&loc.id))
                .collect();
        }
        // Cold or growth-stale: do not false-miss symbols present only in nodes.
        if self.name_index.is_empty() || self.name_index_is_stale() {
            let hits: Vec<&BlockInfo> = self
                .nodes
                .values()
                .filter(|b| b.name == name)
                .collect();
            if !hits.is_empty() && !self.name_index.is_empty() {
                // Stale warm index missed a live name — one-line breadcrumb (not every miss).
                eprintln!(
                    "⚠️ name_index STALE miss recovered: name={name:?} hits={} nodes={} stamp={}",
                    hits.len(),
                    self.nodes.len(),
                    self.name_index_nodes_len
                );
            }
            return hits;
        }
        // Fresh warm index: real miss.
        vec![]
    }

    /// Full O(n) integrity check: every named node appears in the index under the
    /// correct name; every index location id exists and name matches.
    ///
    /// Use after load / Complete stamp / rebuild when you need logged proof of accuracy
    /// (not on the hot Trace path).
    pub fn audit_name_index(&self) -> NameIndexAudit {
        let mut named_nodes = 0usize;
        let mut missing_from_index = 0usize;
        let mut name_mismatches = 0usize;
        let mut sample_missing: Vec<String> = Vec::new();

        for b in self.nodes.values() {
            if b.name.is_empty() {
                continue;
            }
            named_nodes += 1;
            match self.name_index.get(&b.name) {
                None => {
                    missing_from_index += 1;
                    if sample_missing.len() < 5 {
                        sample_missing.push(format!("{}@{}", b.name, b.file.display()));
                    }
                }
                Some(locs) => {
                    if !locs.iter().any(|l| l.id == b.id) {
                        missing_from_index += 1;
                        if sample_missing.len() < 5 {
                            sample_missing.push(format!("{}@{}", b.name, b.file.display()));
                        }
                    }
                }
            }
        }

        let mut indexed_locs = 0usize;
        let mut orphan_locs = 0usize;
        for (key, locs) in &self.name_index {
            for loc in locs {
                indexed_locs += 1;
                match self.nodes.get(&loc.id) {
                    None => orphan_locs += 1,
                    Some(b) => {
                        if b.name != loc.name || loc.name != *key {
                            name_mismatches += 1;
                        }
                    }
                }
            }
        }

        NameIndexAudit {
            nodes_len: self.nodes.len(),
            stamp_nodes_len: self.name_index_nodes_len,
            name_keys: self.name_index.len(),
            named_nodes,
            indexed_locs,
            missing_from_index,
            orphan_locs,
            name_mismatches,
            sample_missing,
        }
    }

    /// Run [`audit_name_index`] and print OK / STALE. Returns whether index is accurate.
    ///
    /// Call after load finalize, Complete stamp, or explicit rebuild when verifying
    /// warehouse quality (docker logs / CI).
    pub fn log_name_index_audit(&self, ctx: &str) -> bool {
        let a = self.audit_name_index();
        if a.is_ok() {
            println!(
                "📇 name_index OK ({ctx}): nodes={} named={} keys={} locs={} stamp={}",
                a.nodes_len, a.named_nodes, a.name_keys, a.indexed_locs, a.stamp_nodes_len
            );
            true
        } else {
            println!(
                "⚠️ name_index STALE ({ctx}): nodes={} stamp={} named={} indexed={} missing={} orphans={} mismatches={} sample={:?}",
                a.nodes_len,
                a.stamp_nodes_len,
                a.named_nodes,
                a.indexed_locs,
                a.missing_from_index,
                a.orphan_locs,
                a.name_mismatches,
                a.sample_missing
            );
            false
        }
    }

    /// Stamp after loading a trusted on-disk index that already matches `nodes`
    /// (skips full rebuild). Still runs a cheap size stamp; call
    /// [`log_name_index_audit`] when you need full proof.
    pub fn stamp_name_index_after_load(&mut self) {
        if self.name_index.is_empty() && !self.nodes.is_empty() {
            self.rebuild_name_index();
            return;
        }
        self.name_index_nodes_len = self.nodes.len();
        // name_index.bin does not carry file→ids — build path index for Arch collect.
        if !self.file_node_index_is_warm() {
            self.rebuild_file_node_index_only();
        }
    }

    /// Unique source files that define/contain exact `name` (for surgical JIT).
    pub fn files_for_name(&self, name: &str) -> Vec<std::path::PathBuf> {
        let mut files: Vec<std::path::PathBuf> = self
            .blocks_for_name(name)
            .into_iter()
            .map(|b| b.file.clone())
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// CALL reverse for a seed, unioning reverse edges of same-name function-like peers.
    ///
    /// Prefer [`Self::name_peer_callers`] for Trace dossiers — agents must not treat
    /// peer reverse as hard CALL into ★. This union remains for twin-id recovery
    /// diagnostics and spine fallbacks that explicitly want the merged set.
    pub fn callers_including_name_peers(&self, seed: &BlockInfo) -> Vec<Id> {
        let mut out = self.callers(&seed.id);
        let mut seen: std::collections::HashSet<Id> = out.iter().cloned().collect();
        for (cid, _) in self.name_peer_callers(seed) {
            if seen.insert(cid.clone()) {
                out.push(cid);
            }
        }
        out
    }

    /// Callers of **other** same-name function-like defs that are **not** direct
    /// CALL parents of `seed`.
    ///
    /// Repo-agnostic twin recovery: call-name maps may edge into peer id B while
    /// Trace ★ is id A. Returns `(caller_id, peer_def_id)` so the dossier can label
    /// “calls a different function with the same name” instead of lying as CALL→★.
    pub fn name_peer_callers(&self, seed: &BlockInfo) -> Vec<(Id, Id)> {
        if !Self::is_function_like_kind(&seed.kind) {
            return Vec::new();
        }
        let direct: std::collections::HashSet<Id> =
            self.callers(&seed.id).into_iter().collect();
        let seed_file = seed.file.to_string_lossy().replace('\\', "/");
        let mut out: Vec<(Id, Id)> = Vec::new();
        let mut seen_caller: std::collections::HashSet<Id> = direct.clone();
        for peer in self.blocks_for_name(&seed.name) {
            if peer.id == seed.id || !Self::is_function_like_kind(&peer.kind) {
                continue;
            }
            // Same-file twins (C++ header overloads / template noise): not useful
            // twin-id recovery and causes peer∩hard on the same pin.
            let peer_file = peer.file.to_string_lossy().replace('\\', "/");
            if peer_file == seed_file {
                continue;
            }
            for cid in self.callers(&peer.id) {
                if cid == seed.id {
                    continue;
                }
                if seen_caller.insert(cid.clone()) {
                    out.push((cid, peer.id.clone()));
                }
            }
        }
        out
    }

    fn is_function_like_kind(kind: &str) -> bool {
        let k = kind.to_ascii_lowercase();
        k.contains("function_item")
            || k.contains("function_definition")
            || k.contains("function_declaration")
            || k.contains("method_definition")
            || k.contains("method_declaration")
            || k.contains("method_item")
            || k.contains("async_function")
    }

}

#[cfg(test)]
mod name_index_tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn blk(name: &str, file: &str, line: usize) -> BlockInfo {
        let hash = format!("{name}{line:0>8}");
        let hash = format!("{hash:0<16}");
        BlockInfo {
            id: Id::new(file, "function_item", &hash),
            name: name.into(),
            file: PathBuf::from(file),
            kind: "function_item".into(),
            lang: "rust".into(),
            start_line: line,
            end_line: line + 1,
            start_byte: 0,
            end_byte: 1,
            parent_id: None,
            children: vec![],
            content_hash: hash,
            sig_hash: "s".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 0.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn rebuild_name_index_lists_all_homonyms() {
        let mut g = CodeGraph::new();
        g.nodes.insert(
            Id::new("a.rs", "function_item", "aaaaaaaa"),
            blk("main", "a.rs", 1),
        );
        // second main needs distinct id/hash
        let b = blk("main", "b.rs", 10);
        g.nodes.insert(b.id.clone(), b);
        g.rebuild_name_index();
        let locs = g.locations_for_name("main");
        assert_eq!(locs.len(), 2);
        assert!(locs.iter().any(|l| l.file.ends_with("a.rs")));
        assert!(locs.iter().any(|l| l.file.ends_with("b.rs")));
    }

    #[test]
    fn multi_file_name_collisions_ranks_by_file_count() {
        let mut g = CodeGraph::new();
        for (name, file, line) in [
            ("Foo", "a.rs", 1),
            ("Foo", "b.rs", 2),
            ("Bar", "a.rs", 3),
            ("Bar", "b.rs", 4),
            ("Bar", "c.rs", 5),
            ("solo", "a.rs", 6),
        ] {
            let b = blk(name, file, line);
            g.nodes.insert(b.id.clone(), b);
        }
        g.rebuild_name_index();
        let cols = g.multi_file_name_collisions(2, 10, 2);
        assert!(cols.iter().any(|(n, _, f)| n == "Bar" && *f == 3));
        assert!(cols.iter().any(|(n, _, f)| n == "Foo" && *f == 2));
        assert!(!cols.iter().any(|(n, _, _)| n == "solo"));
        // Bar ranks first (more files)
        assert_eq!(cols[0].0, "Bar");
    }

    #[test]
    fn multi_file_name_collisions_drops_junk_unknown_and_tests() {
        let mut g = CodeGraph::new();
        for (name, file, line) in [
            ("unknown", "a.rs", 1),
            ("unknown", "b.rs", 2),
            ("unknown", "c.rs", 3),
            ("tests", "a.rs", 4),
            ("tests", "b.rs", 5),
            ("Default", "a.rs", 6),
            ("Default", "b.rs", 7),
            ("ab", "a.rs", 8), // alnum_len < 3 via is_junk_symbol_name
            ("ab", "b.rs", 9),
        ] {
            let b = blk(name, file, line);
            g.nodes.insert(b.id.clone(), b);
        }
        g.rebuild_name_index();
        let cols = g.multi_file_name_collisions(2, 20, 2);
        assert!(
            cols.iter().any(|(n, _, _)| n == "Default"),
            "real collision kept: {cols:?}"
        );
        assert!(
            !cols.iter().any(|(n, _, _)| n == "unknown"),
            "unknown junk filtered: {cols:?}"
        );
        assert!(
            !cols.iter().any(|(n, _, _)| n == "tests"),
            "tests junk filtered: {cols:?}"
        );
        assert!(
            !cols.iter().any(|(n, _, _)| n == "ab"),
            "short junk filtered: {cols:?}"
        );
    }

    #[test]
    fn blocks_for_name_uses_index_no_false_miss_scan() {
        let mut g = CodeGraph::new();
        let b = blk("Entity", "ecs.rs", 1);
        g.nodes.insert(b.id.clone(), b);
        g.rebuild_name_index();
        assert_eq!(g.blocks_for_name("Entity").len(), 1);
        assert!(g.blocks_for_name("NoSuchThing").is_empty());
        // Warm index: miss must not invent hits
        assert!(g.files_for_name("NoSuchThing").is_empty());
        let files = g.files_for_name("Entity");
        assert_eq!(files, vec![PathBuf::from("ecs.rs")]);
    }

    #[test]
    fn ensure_name_index_rebuilds_on_node_growth() {
        let mut g = CodeGraph::new();
        let a = blk("Alpha", "a.rs", 1);
        g.nodes.insert(a.id.clone(), a);
        g.rebuild_name_index();
        assert_eq!(g.name_index_nodes_len, 1);
        assert!(!g.name_index_is_stale());

        // Growth without rebuild → stale stamp (click Group false-miss class).
        let b = blk("Group", "group.rs", 1);
        g.nodes.insert(b.id.clone(), b);
        assert!(g.name_index_is_stale());
        // Fallback must recover the live name before ensure.
        assert_eq!(g.blocks_for_name("Group").len(), 1);

        g.ensure_name_index();
        assert!(!g.name_index_is_stale());
        assert_eq!(g.name_index_nodes_len, 2);
        assert_eq!(g.locations_for_name("Group").len(), 1);
        assert_eq!(g.blocks_for_name("Group").len(), 1);
        // Fresh warm miss still empty.
        assert!(g.blocks_for_name("NoSuch").is_empty());
    }

    #[test]
    fn audit_name_index_detects_stale_and_ok_after_rebuild() {
        let mut g = CodeGraph::new();
        let a = blk("Alpha", "a.rs", 1);
        let b = blk("Group", "g.rs", 2);
        let a_id = a.id.clone();
        g.nodes.insert(a.id.clone(), a);
        g.nodes.insert(b.id.clone(), b);
        // Manually partial index (simulates lagging name_index.bin): Alpha only.
        g.name_index.insert(
            "Alpha".into(),
            vec![NameLocation {
                id: a_id,
                name: "Alpha".into(),
                file: PathBuf::from("a.rs"),
                start_line: 1,
                end_line: 2,
                kind: "function_item".into(),
                lang: "rust".into(),
            }],
        );
        g.name_index_nodes_len = 2; // lie: stamp matches count but content incomplete
        let bad = g.audit_name_index();
        assert!(!bad.is_ok());
        assert!(bad.missing_from_index >= 1);

        g.rebuild_name_index();
        let ok = g.audit_name_index();
        assert!(ok.is_ok(), "{ok:?}");
        assert!(g.log_name_index_audit("unit test"));
    }

    #[test]
    fn warm_fresh_index_miss_does_not_scan() {
        let mut g = CodeGraph::new();
        let a = blk("OnlyOne", "a.rs", 1);
        g.nodes.insert(a.id.clone(), a);
        g.rebuild_name_index();
        assert!(!g.name_index_is_stale());
        // Name not in nodes → empty without inventing.
        assert!(g.blocks_for_name("Ghost").is_empty());
    }

    #[test]
    fn snapshot_for_publish_preserves_nodes_when_stripped() {
        let mut g = CodeGraph::new();
        let a = blk("Alpha", "a.rs", 1);
        g.nodes.insert(a.id.clone(), a);
        g.rebuild_name_index();
        assert!(g.sources_stripped());
        let snap = g.snapshot_for_publish();
        assert_eq!(snap.nodes.len(), 1);
        assert!(snap.sources_stripped());
        assert_eq!(snap.blocks_for_name("Alpha").len(), 1);
    }
}

