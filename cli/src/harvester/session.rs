//! Shared harvest protocol: next_cards + commit_emit.
//! Used by agent_loop (litellm), file mailbox, and MCP — one truth for gold labels.

use super::cards::{format_cards_for_prompt, is_stub_note, NeighborhoodCard};
use super::frontier::{next_cards_with_budget, SeedStrategy};
use super::source::Source;
use super::state::HarvestState;
use super::template::Template;
use super::tools::ToolRegistry;
use super::types::{FatEdge, FatNode};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Package handed to any labeler (LLM / agent / mailbox).
#[derive(Debug, Clone, Serialize)]
pub struct CardBatch {
    pub query: String,
    pub step: usize,
    pub cards: Vec<NeighborhoodCard>,
    pub batch_min_criticals: usize,
    pub batch_min_rejections: usize,
    pub catch_up: String,
    pub rules: String,
    pub status: HarvestStatus,
    /// Human prompt (same shape as litellm path) for agents that want text.
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarvestStatus {
    pub nodes: usize,
    pub criticals: usize,
    pub rejections: usize,
    pub target_criticals: usize,
    pub target_rejections: usize,
    pub goals_met: bool,
    pub step: usize,
    pub max_steps: usize,
    pub fat_path: String,
    pub repo: String,
    pub query: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitResult {
    pub ok: bool,
    pub issues: Vec<String>,
    pub added_criticals: usize,
    pub added_rejections: usize,
    pub capped: usize,
    pub status: HarvestStatus,
}

pub struct HarvestSession {
    pub tpl: Template,
    pub source: Source,
    pub tools: ToolRegistry,
    pub state: HarvestState,
    pub fat_path: PathBuf,
    pub step: usize,
    /// Cards from last `next_cards` — commit validates against these.
    pub active_cards: Vec<NeighborhoodCard>,
    strategy: SeedStrategy,
}

impl HarvestSession {
    pub fn open(
        tpl: Template,
        source: Source,
        fat_path: PathBuf,
        load_previous: bool,
    ) -> Self {
        if !load_previous && fat_path.exists() {
            let _ = std::fs::remove_file(&fat_path);
        }
        let mut state = HarvestState::load_or_new(&fat_path, &tpl.query);
        state.current.model = tpl.llm.model.clone();
        state.current.repo = tpl.repo.clone();

        if tpl.polyglot.include_interconnect {
            // P.6 / Phase 8: typed runtime bridges only (export|ipc|twin), not CALL soup.
            for (from, to, edge_type, reason) in source.interconnect_edges() {
                if !state.current.interconnect_edges.iter().any(|e| {
                    e.from == from && e.to == to && e.edge_type == edge_type
                }) {
                    state.current.interconnect_edges.push(FatEdge {
                        from,
                        to,
                        edge_type,
                        reason,
                    });
                }
            }
        }
        if !tpl.focus.ignore_paths.is_empty() {
            state.current.interconnect_edges.retain(|e| {
                let fp = e.from.split(':').next().unwrap_or("");
                let tp = e.to.split(':').next().unwrap_or("");
                !tpl.focus
                    .ignore_paths
                    .iter()
                    .any(|s| fp.contains(s) || tp.contains(s))
            });
        }

        let strategy = SeedStrategy::parse(&tpl.frontier.strategy);
        let tools = ToolRegistry::with_source(source.clone());
        // Warm graph once
        let n = source.load_code_graph().map(|g| g.nodes.len()).unwrap_or(0);
        let (n_export, n_ipc, n_twin) = {
            let mut e = 0usize;
            let mut i = 0usize;
            let mut t = 0usize;
            for edge in &state.current.interconnect_edges {
                match edge.edge_type.as_str() {
                    "export" => e += 1,
                    "ipc" => i += 1,
                    "twin" => t += 1,
                    _ => {}
                }
            }
            (e, i, t)
        };
        live_log(&format!(
            "[harvest-session] open repo={} fat={} blocks={} strategy={:?} interconnect={{export:{},ipc:{},twin:{}}}",
            tpl.repo,
            fat_path.display(),
            n,
            strategy,
            n_export,
            n_ipc,
            n_twin
        ));

        Self {
            tpl,
            source,
            tools,
            state,
            fat_path,
            step: 0,
            active_cards: vec![],
            strategy,
        }
    }

    pub fn status(&self) -> HarvestStatus {
        let c = self.state.current.critical_node_ids.len();
        let r = self.state.current.rejected_node_ids.len();
        HarvestStatus {
            nodes: self.state.current.nodes.len(),
            criticals: c,
            rejections: r,
            target_criticals: self.tpl.incremental.target_criticals,
            target_rejections: self.tpl.incremental.effective_target_rejections(),
            goals_met: self.tpl.incremental.goals_met(c, r),
            step: self.step,
            max_steps: self.tpl.incremental.max_steps,
            fat_path: self.fat_path.to_string_lossy().into_owned(),
            repo: self.tpl.repo.clone(),
            query: self.tpl.query.clone(),
        }
    }

    pub fn batch_mins(&self) -> (usize, usize) {
        self.tpl.incremental.batch_polarity_mins(
            self.state.current.critical_node_ids.len(),
            self.state.current.rejected_node_ids.len(),
            self.tpl.accuracy.min_criticals_per_batch,
            self.tpl.accuracy.min_hard_negatives_per_batch,
        )
    }

    /// Next neighborhood cards for labeling (identical for MCP / mailbox / litellm).
    pub fn next_card_batch(&mut self) -> Result<CardBatch, String> {
        if self.tpl.incremental.goals_met(
            self.state.current.critical_node_ids.len(),
            self.state.current.rejected_node_ids.len(),
        ) {
            return Err("goals already met".into());
        }
        if self.step >= self.tpl.incremental.max_steps {
            return Err("max_steps reached".into());
        }

        let cards = next_cards_with_budget(
            &self.state.current,
            self.tpl.incremental.batch_size,
            &self.tpl.query,
            &self.source,
            &self.tpl.focus.scope_paths,
            &self.tpl.focus.ignore_paths,
            self.strategy,
            self.tpl.frontier.use_degree || self.tpl.focus.prefer_high_degree,
            self.tpl.frontier.card_budget(),
        );
        if cards.is_empty() {
            return Err("no unvisited candidates in scope".into());
        }

        let (batch_min_c, batch_min_r) = self.batch_mins();
        let catch_up = if batch_min_r == 0 && batch_min_c > 0 {
            "CATCH-UP: rejections at/over target — emit CRITICALS only (extra rejects dropped)."
                .into()
        } else if batch_min_c == 0 && batch_min_r > 0 {
            "CATCH-UP: criticals at/over target — emit hard NEGATIVES only (extra crits dropped)."
                .into()
        } else {
            String::new()
        };
        let rules = accuracy_rules_text(&self.tpl, batch_min_c, batch_min_r);
        let status = self.status();
        let state_summary = format!(
            "State: {} nodes ({} critical, {} rejected)",
            status.nodes, status.criticals, status.rejections
        );
        let prompt = build_labeler_prompt(
            &self.tpl.query,
            &state_summary,
            &cards,
            &rules,
            &catch_up,
        );

        self.active_cards = cards.clone();
        live_log(&format!(
            "[harvest-session] next_cards step={} n={} seeds={:?}",
            self.step,
            cards.len(),
            cards
                .iter()
                .map(|c| format!("{}:{}", c.seed_reason, c.center_name))
                .collect::<Vec<_>>()
        ));

        Ok(CardBatch {
            query: self.tpl.query.clone(),
            step: self.step,
            cards,
            batch_min_criticals: batch_min_c,
            batch_min_rejections: batch_min_r,
            catch_up,
            rules,
            status,
            prompt,
        })
    }

    /// Commit labeler emit (fail-closed).
    ///
    /// Pipeline: sanitize/dedup → polarity caps → validate → persist.
    /// **Advances `step` only on success.** Failed batches must call
    /// [`Self::advance_after_failed_batch`] (agent loop) so the frontier moves.
    pub fn commit_emit(&mut self, raw_nodes: &[Value]) -> CommitResult {
        let (batch_min_c, batch_min_r) = self.batch_mins();
        let cards = self.active_cards.clone();

        if raw_nodes.is_empty() {
            return CommitResult {
                ok: false,
                issues: vec!["empty_emit".into()],
                added_criticals: 0,
                added_rejections: 0,
                capped: 0,
                status: self.status(),
            };
        }

        let mut pending = sanitize_dedup_pending(raw_nodes, &self.state.current.nodes);
        let capped = apply_polarity_caps(
            &mut pending,
            self.tpl
                .incremental
                .cap_new_criticals(self.state.current.critical_node_ids.len()),
            self.tpl
                .incremental
                .cap_new_rejections(self.state.current.rejected_node_ids.len()),
        );

        let issues = validate_emit_batch(&pending, &self.tpl, &cards, batch_min_c, batch_min_r);
        if !issues.is_empty() {
            live_log(&format!(
                "[harvest-session] commit REJECTED: {:?}",
                issues
            ));
            return CommitResult {
                ok: false,
                issues,
                added_criticals: 0,
                added_rejections: 0,
                capped,
                status: self.status(),
            };
        }

        let (crit, rej) = self.persist_accepted_batch(pending);
        live_log(&format!(
            "[harvest-session] commit OK +{} crit +{} rej (total n={} c={} r={})",
            crit,
            rej,
            self.state.current.nodes.len(),
            self.state.current.critical_node_ids.len(),
            self.state.current.rejected_node_ids.len()
        ));

        CommitResult {
            ok: true,
            issues: vec![],
            added_criticals: crit,
            added_rejections: rej,
            capped,
            status: self.status(),
        }
    }

    /// When a batch ends without a successful [`Self::commit_emit`], bump `step`
    /// so the next card draw is not stuck on the same centers forever.
    /// Success path advances step inside `commit_emit` only.
    pub fn advance_after_failed_batch(&mut self) {
        self.step += 1;
    }

    fn persist_accepted_batch(&mut self, pending: Vec<FatNode>) -> (usize, usize) {
        let mut crit = 0usize;
        let mut rej = 0usize;
        for node in pending {
            if node.rejection_reason.is_some() {
                self.state.current.rejected_node_ids.push(node.id.clone());
                rej += 1;
            } else if node.is_critical {
                self.state.current.critical_node_ids.push(node.id.clone());
                crit += 1;
            }
            self.state.current.nodes.push(node);
        }
        self.state.save();
        self.step += 1;
        self.active_cards.clear();
        (crit, rej)
    }

    /// Repo-relative tool for agents (read_file / grep) under harvest root.
    pub fn tool(&self, action: &str, args: &Value) -> Value {
        self.tools.dispatch(action, args)
    }

    pub fn finalize(&mut self) {
        // Dedup + rebuild lists (same as agent_loop end)
        let mut seen = std::collections::HashSet::new();
        self.state.current.nodes.retain(|n| seen.insert(n.id.clone()));
        let mut new_crit = vec![];
        let mut new_rej = vec![];
        for n in &self.state.current.nodes {
            if n.rejection_reason.is_some() {
                new_rej.push(n.id.clone());
            } else if n.is_critical {
                new_crit.push(n.id.clone());
            }
        }
        self.state.current.critical_node_ids = new_crit;
        self.state.current.rejected_node_ids = new_rej;
        self.state.save();
        let s = self.status();
        live_log(&format!(
            "[harvest-session] finalize nodes={} criticals={} rejections={}",
            s.nodes, s.criticals, s.rejections
        ));
    }
}

// ─── validate / sanitize (shared) ───────────────────────────────────────────

/// LLM JSON → [`FatNode`]. Drop-nulls and fill defaults so partial emits still type.
///
/// Fallback contract (intentional, not silent bugs):
/// | Field empty / null | Default |
/// |--------------------|---------|
/// | id                 | reject (`None`) |
/// | node_type          | `"block"` |
/// | name               | id segment after first `:` (usually Tree-sitter **kind**, last resort) |
/// | file               | id prefix before first `:` (`file:kind:8hex` layout) |
/// | range              | `"0-0"` |
/// | snippet            | `"..."` |
/// | rejection_reason set | **`is_critical = false`** (reject wins polarity; XOR for gold) |
///
/// Id layout is Butler `file:kind:8hex` — hash is hex (no colons). Paths with `:`
/// can confuse file/name recovery; prefer LLM-filled `file`/`name` fields.
pub fn sanitize_fat_node(n: &Value) -> Option<FatNode> {
    let mut node_val = n.clone();
    if let Some(obj) = node_val.as_object_mut() {
        for k in [
            "range",
            "snippet",
            "node_type",
            "name",
            "file",
            "exploration_note",
        ] {
            if let Some(v) = obj.get_mut(k) {
                if v.is_null() {
                    *v = json!("");
                }
            }
        }
    }
    let mut node: FatNode = serde_json::from_value(node_val).ok()?;
    if node.id.trim().is_empty() {
        return None;
    }
    if node.node_type.is_empty() {
        node.node_type = "block".into();
    }
    if node.name.is_empty() {
        node.name = node
            .id
            .split(':')
            .nth(1)
            .unwrap_or(&node.id)
            .to_string();
    }
    if node.file.is_empty() {
        node.file = node.id.split(':').next().unwrap_or("").to_string();
    }
    if node.range.is_empty() || node.range == "unknown" {
        node.range = "0-0".into();
    }
    if node.snippet.is_empty() {
        node.snippet = "...".into();
    }
    if node
        .rejection_reason
        .as_ref()
        .is_some_and(|r| !r.trim().is_empty())
    {
        // Polarity: hard-negative wins over is_critical=true from messy LLM JSON.
        node.is_critical = false;
    }
    Some(node)
}

/// Sanitize raw emit nodes and drop ids already in the fat or this batch.
fn sanitize_dedup_pending(raw_nodes: &[Value], existing: &[FatNode]) -> Vec<FatNode> {
    let mut pending: Vec<FatNode> = Vec::new();
    for n in raw_nodes {
        let Some(node) = sanitize_fat_node(n) else {
            continue;
        };
        if existing.iter().any(|x| x.id == node.id) {
            continue;
        }
        if pending.iter().any(|x| x.id == node.id) {
            continue;
        }
        pending.push(node);
    }
    pending
}

/// Drop new labels on poles already at/over soft targets. Returns count removed.
fn apply_polarity_caps(pending: &mut Vec<FatNode>, cap_c: bool, cap_r: bool) -> usize {
    if !cap_c && !cap_r {
        return 0;
    }
    let before = pending.len();
    pending.retain(|n| {
        let is_rej = n
            .rejection_reason
            .as_ref()
            .is_some_and(|r| !r.trim().is_empty());
        if is_rej && cap_r {
            return false;
        }
        if !is_rej && n.is_critical && cap_c {
            return false;
        }
        true
    });
    before.saturating_sub(pending.len())
}

pub fn validate_emit_batch(
    pending: &[FatNode],
    tpl: &Template,
    _cards: &[NeighborhoodCard],
    min_criticals: usize,
    min_rejections: usize,
) -> Vec<String> {
    let mut issues = Vec::new();
    if pending.is_empty() {
        issues.push("empty_pending".into());
        return issues;
    }
    let mut crit = 0usize;
    let mut rej = 0usize;
    for n in pending {
        if n.id.trim().is_empty() {
            issues.push("empty_id".into());
        }
        if tpl.accuracy.require_exploration_note && n.exploration_note.trim().is_empty() {
            issues.push(format!("missing_note:{}", n.name));
        }
        if tpl.accuracy.ban_stub_notes && is_stub_note(&n.exploration_note) {
            issues.push(format!("stub_note:{}", n.name));
        }
        let is_rej = n
            .rejection_reason
            .as_ref()
            .is_some_and(|r| !r.trim().is_empty());
        if is_rej {
            rej += 1;
            if tpl.accuracy.ban_stub_notes {
                if let Some(r) = &n.rejection_reason {
                    if is_stub_note(r) {
                        issues.push(format!("stub_rejection:{}", n.name));
                    }
                }
            }
        } else if n.is_critical {
            crit += 1;
        } else if tpl.accuracy.require_label_polarity {
            issues.push(format!("no_polarity:{}", n.name));
        }
    }
    if crit < min_criticals {
        issues.push(format!(
            "min_criticals (got {}, want {})",
            crit, min_criticals
        ));
    }
    if min_rejections > 0 && tpl.accuracy.require_explicit_rejections && rej == 0 {
        issues.push("require_explicit_rejections".into());
    }
    if rej < min_rejections {
        issues.push(format!(
            "min_hard_negatives (got {}, want {})",
            rej, min_rejections
        ));
    }
    issues
}

fn accuracy_rules_text(tpl: &Template, batch_min_c: usize, batch_min_r: usize) -> String {
    format!(
        "ACCURACY (fail-closed):\n- min_criticals_this_batch: {}\n- min_hard_negatives_this_batch: {}\n- ban_stub_notes: {}\n- require_label_polarity: {}\nNotes must explain CODE role for the QUERY.",
        batch_min_c,
        batch_min_r,
        tpl.accuracy.ban_stub_notes,
        tpl.accuracy.require_label_polarity,
    )
}

fn build_labeler_prompt(
    query: &str,
    state_summary: &str,
    cards: &[NeighborhoodCard],
    rules: &str,
    catch_up: &str,
) -> String {
    let cards_json = format_cards_for_prompt(cards);
    let centers: Vec<_> = cards
        .iter()
        .map(|c| format!("{} ({})", c.center_id, c.center_name))
        .collect();
    // Mission-first: reasoning models must spend completion budget on a finished
    // emit_batch, not on re-stating rules or drafting JSON twice in private CoT.
    // Protocol note: parser strips ``` fences if the model wraps JSON (`llm.rs`);
    // still prefer raw JSON so extraction is one object, not prose + fence noise.
    format!(
        r#"MISSION: stamp gold GNN training labels on CARDS (not explore the repo — Butler already navigated).
For each center: critical if it advances QUERY, else hard-negative reject. Cards ARE the menu — never invent ids.
QUERY: {query}
{state_summary}
CARDS:
{cards_json}
Centers (label all): {centers:?}
{rules}
{catch_up}
THINK (if you reason privately): short — polarity + role per card only. Do NOT restate this prompt, re-list cards, or draft the full JSON in thinking then again in the answer. Incomplete JSON = failed batch.
DELIVERABLE: one complete JSON object only (prefer no markdown fences; fences are stripped if present; no prose after):
{{"action":"emit_batch","args":{{"nodes":[{{"id":"...","node_type":"...","name":"...","file":"...","range":"...","snippet":"...","exploration_note":"...","is_critical":true}}]}}}}
Each node: is_critical=true OR rejection_reason set (not both — reject wins). exploration_note = one sentence: code role for QUERY.
"#
    )
}

pub fn live_log(line: &str) {
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/harvester_live.log")
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "{line}")
        });
}

// ─── Global session for MCP (one active harvest) ────────────────────────────

use std::sync::Mutex;
static GLOBAL: Mutex<Option<HarvestSession>> = Mutex::new(None);

pub fn global_open(session: HarvestSession) {
    *GLOBAL.lock().unwrap() = Some(session);
}

pub fn global_with<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&mut HarvestSession) -> R,
{
    let mut g = GLOBAL.lock().map_err(|e| e.to_string())?;
    let s = g.as_mut().ok_or_else(|| {
        "no active harvest session — call harvest_open first".to_string()
    })?;
    Ok(f(s))
}

pub fn global_close() -> Option<HarvestStatus> {
    let mut g = GLOBAL.lock().ok()?;
    if let Some(mut s) = g.take() {
        s.finalize();
        Some(s.status())
    } else {
        None
    }
}

pub fn global_status() -> Result<HarvestStatus, String> {
    global_with(|s| s.status())
}

/// Build template from MCP/CLI args for open.
pub fn template_from_open_args(
    repo: &str,
    query: &str,
    model: &str,
    batch_size: usize,
    max_steps: usize,
    target_criticals: usize,
    target_rejections: usize,
    scope: Vec<String>,
    butler_export: Option<String>,
    card_profile: &str,
) -> Template {
    use super::template::*;
    // Slow profile: smaller batches by default if caller left batch_size at 0.
    let batch = if batch_size == 0 {
        if card_profile.eq_ignore_ascii_case("slow") {
            2
        } else {
            4
        }
    } else {
        batch_size
    };
    Template {
        name: "agent-session".into(),
        query: query.into(),
        repo: repo.into(),
        butler_export,
        output: Output {
            schema: "full_fat_v1".into(),
            format_version: 1,
        },
        incremental: Incremental {
            batch_size: batch,
            max_steps,
            save_after_each: true,
            load_previous_context: true,
            target_criticals,
            target_rejections,
        },
        accuracy: Accuracy::default(),
        focus: Focus {
            scope_paths: scope,
            ignore_paths: vec![
                "target".into(),
                "tests".into(),
                "node_modules".into(),
            ],
            prefer_high_degree: false,
        },
        frontier: Frontier {
            strategy: "neighborhood".into(),
            use_ast_distance: false,
            use_degree: false,
            use_bm25: false,
            card_profile: card_profile.into(),
            max_neighbors: 0,
            max_snippet_chars: 0,
        },
        llm: Llm {
            via: "agent".into(),
            model: model.into(),
            temperature: 0.1,
        },
        polyglot: Polyglot {
            include_interconnect: true,
        },
    }
}

/// Mailbox one-shot: write card batch request, wait for emit response, commit.
pub fn mailbox_step(
    session: &mut HarvestSession,
    mailbox_dir: &Path,
    timeout_secs: u64,
    cancel: Option<&AtomicBool>,
) -> Result<CommitResult, String> {
    std::fs::create_dir_all(mailbox_dir).map_err(|e| e.to_string())?;
    let batch = session.next_card_batch()?;
    let req_path = mailbox_dir.join("request.json");
    let resp_path = mailbox_dir.join("response.json");
    let _ = std::fs::remove_file(&resp_path);
    let payload = json!({
        "protocol": "butler_harvest_v1",
        "instruction": "MISSION: gold GNN labels on batch cards only. Complete emit_batch JSON for every center (critical or rejection_reason). One-sentence notes for QUERY. No invented ids.",
        "batch": batch,
    });
    std::fs::write(
        &req_path,
        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    live_log(&format!(
        "[mailbox] wrote {} — waiting for {}",
        req_path.display(),
        resp_path.display()
    ));

    let start = std::time::Instant::now();
    loop {
        if cancel.map_or(false, |c| c.load(Ordering::Relaxed)) {
            return Err("cancelled".into());
        }
        if resp_path.exists() {
            let raw = std::fs::read_to_string(&resp_path).map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let nodes = v
                .pointer("/args/nodes")
                .or_else(|| v.get("nodes"))
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            let result = session.commit_emit(&nodes);
            let _ = std::fs::remove_file(&resp_path);
            return Ok(result);
        }
        if start.elapsed().as_secs() > timeout_secs {
            return Err(format!(
                "mailbox timeout after {timeout_secs}s waiting for {}",
                resp_path.display()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}
