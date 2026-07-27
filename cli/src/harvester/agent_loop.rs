//! Incremental agent loop: shared session + litellm / mailbox labeler.

use super::llm::LlmClient;
use super::session::{live_log, HarvestSession};
use super::source::Source;
use super::template::Template;
use super::tools::ToolRegistry;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn run_harvest(
    tpl: &Template,
    client: &LlmClient,
    _registry: &ToolRegistry,
    source: &Source,
    fat_path: &Path,
    cancel: Option<&'static AtomicBool>,
) {
    let load_prev = tpl.incremental.load_previous_context;
    let mut session = HarvestSession::open(tpl.clone(), source.clone(), fat_path.to_path_buf(), load_prev);

    if session.status().goals_met {
        live_log(&format!(
            "[harvester] Goals already met (crit={} rej={}); nothing to do.",
            session.status().criticals,
            session.status().rejections
        ));
        session.finalize();
        return;
    }

    // Mailbox mode: llm base is mailbox:/path or agent:mailbox:/path
    if let Some(dir) = client.mailbox_dir() {
        let timeout = std::env::var("HARVESTER_MAILBOX_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600u64);
        live_log(&format!(
            "[harvester] MAILBOX mode dir={} timeout={}s",
            dir.display(),
            timeout
        ));
        while session.step < session.tpl.incremental.max_steps {
            if cancel.map_or(false, |c| c.load(Ordering::Relaxed)) {
                break;
            }
            if session.status().goals_met {
                live_log("[harvester] Soft goals met — stopping.");
                break;
            }
            match super::session::mailbox_step(
                &mut session,
                &dir,
                timeout,
                cancel.map(|c| c as &AtomicBool),
            ) {
                Ok(r) if r.ok => {
                    eprintln!(
                        "[harvester] mailbox commit +{} crit +{} rej",
                        r.added_criticals, r.added_rejections
                    );
                }
                Ok(r) => {
                    live_log(&format!(
                        "[harvester] mailbox commit failed: {:?} — rewrite response.json",
                        r.issues
                    ));
                    // leave response for debugging; next loop rewrites request
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                Err(e) if e.contains("goals") || e.contains("max_steps") || e.contains("no unvisited") => {
                    live_log(&format!("[harvester] stop: {e}"));
                    break;
                }
                Err(e) => {
                    live_log(&format!("[harvester] mailbox error: {e}"));
                    break;
                }
            }
        }
        session.finalize();
        print_summary(&session);
        return;
    }

    // Litellm / stub path via session cards + commit
    while session.step < session.tpl.incremental.max_steps {
        if cancel.map_or(false, |c| c.load(Ordering::Relaxed)) {
            eprintln!("[harvester] Cancelled");
            break;
        }
        if session.status().goals_met {
            live_log("[harvester] Soft goals met — stopping.");
            break;
        }

        let batch = match session.next_card_batch() {
            Ok(b) => b,
            Err(e) => {
                live_log(&format!("[harvester] {e}"));
                break;
            }
        };

        let mut action_log: Vec<String> = vec![];
        let mut batch_done = false;
        const MAX_TOOL_TURNS: usize = 3;
        const MAX_TURNS: usize = 10;
        let mut tool_turns = 0usize;

        for turn in 0..MAX_TURNS {
            if cancel.map_or(false, |c| c.load(Ordering::Relaxed)) {
                break;
            }
            let force_emit = tool_turns >= MAX_TOOL_TURNS;
            let mut prompt = batch.prompt.clone();
            if !action_log.is_empty() {
                prompt.push_str("\n\nRecent tool results:\n");
                prompt.push_str(&action_log.join("\n"));
            }
            if force_emit {
                prompt.push_str(
                    "\n*** TOOL BUDGET EXHAUSTED — MUST emit_batch now on card centers. ***\n",
                );
            }

            let Some(action) = client.ask(&prompt) else {
                break;
            };
            let action_name = action.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let args = action.get("args").cloned().unwrap_or(serde_json::json!({}));
            eprintln!("[harvester] Got action: {action_name}");
            live_log(&format!(
                "[harvester] Got action: {action_name} (turn={turn} tools={tool_turns})"
            ));

            if force_emit && action_name != "emit_batch" {
                action_log.push(format!(
                    "SYSTEM: must emit_batch; ignored '{action_name}'"
                ));
                continue;
            }

            if action_name == "emit_batch" {
                let nodes = args
                    .get("nodes")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let result = session.commit_emit(&nodes);
                if result.ok {
                    batch_done = true;
                    break;
                }
                action_log.push(format!(
                    "ACCURACY FAILED: {:?}. Fix and emit_batch again.",
                    result.issues
                ));
                if action_log.len() > 6 {
                    action_log.remove(0);
                }
            } else {
                let result = session.tool(action_name, &args);
                action_log.push(super::tools::format_tool_result_for_llm(action_name, &result));
                if action_log.len() > 6 {
                    action_log.remove(0);
                }
                tool_turns += 1;
            }
        }

        if !batch_done {
            live_log(&format!(
                "[harvester] step {} ended without valid emit",
                session.step
            ));
            // commit_emit advances step only on success; move frontier on fail.
            session.advance_after_failed_batch();
        }
    }

    session.finalize();
    print_summary(&session);
}

fn print_summary(session: &HarvestSession) {
    let s = session.status();
    live_log(&format!(
        "[harvester] Finished. nodes={} criticals={} rejections={} (targets c={} r={})",
        s.nodes, s.criticals, s.rejections, s.target_criticals, s.target_rejections
    ));
    if s.criticals == 0 && s.nodes > 0 {
        live_log("[harvester] WARNING: 0 criticals — do NOT train.");
    }
    if s.criticals > 0 && s.rejections > 0 {
        let ratio = s.rejections as f64 / s.criticals as f64;
        if ratio > 4.0 || ratio < 0.25 {
            live_log(&format!(
                "[harvester] WARNING: imbalance rej/crit={ratio:.2}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvester::session::{validate_emit_batch, sanitize_fat_node};
    use crate::harvester::template::{Accuracy, Focus, Frontier, Incremental, Llm, Output, Polyglot, Template};
    use crate::harvester::types::FatNode;
    use std::path::PathBuf;

    fn sample_tpl(accuracy: Accuracy) -> Template {
        Template {
            name: "test".into(),
            query: "core entry".into(),
            repo: "/tmp".into(),
            butler_export: None,
            output: Output {
                schema: "full".into(),
                format_version: 1,
            },
            incremental: Incremental {
                batch_size: 2,
                max_steps: 1,
                save_after_each: true,
                load_previous_context: false,
                target_criticals: 0,
                target_rejections: 0,
            },
            accuracy,
            focus: Focus::default(),
            frontier: Frontier {
                strategy: "neighborhood".into(),
                use_ast_distance: false,
                use_degree: false,
                use_bm25: false,
                card_profile: "fast".into(),
                max_neighbors: 0,
                max_snippet_chars: 0,
            },
            llm: Llm {
                via: "stub".into(),
                model: "stub".into(),
                temperature: 0.0,
            },
            polyglot: Polyglot {
                include_interconnect: false,
            },
        }
    }

    #[test]
    fn validate_rejects_stub() {
        let tpl = sample_tpl(Accuracy::default());
        let pending = vec![FatNode {
            id: "a.rs:function_item:aaaaaaaa".into(),
            name: "a".into(),
            exploration_note: "selected from CodeGraph".into(),
            is_critical: false,
            rejection_reason: Some("peripheral helper".into()),
            ..Default::default()
        }];
        let issues = validate_emit_batch(&pending, &tpl, &[], 1, 1);
        assert!(!issues.is_empty());
    }

    #[test]
    fn validate_accepts_balanced() {
        let tpl = sample_tpl(Accuracy::default());
        let pending = vec![
            FatNode {
                id: "a.rs:function_item:aaaaaaaa".into(),
                name: "alpha".into(),
                exploration_note: "implements the main CLI parse path for the query".into(),
                is_critical: true,
                ..Default::default()
            },
            FatNode {
                id: "b.rs:function_item:bbbbbbbb".into(),
                name: "beta".into(),
                exploration_note: "formatting helper unused by the query path".into(),
                is_critical: false,
                rejection_reason: Some("utility not on critical path for this query".into()),
                ..Default::default()
            },
        ];
        assert!(validate_emit_batch(&pending, &tpl, &[], 1, 1).is_empty());
    }

    #[test]
    fn sanitize_strips_nulls() {
        let v = serde_json::json!({
            "id": "x.rs:function_item:abcd1234",
            "name": null,
            "is_critical": true,
            "exploration_note": "real code path for the query here"
        });
        let n = sanitize_fat_node(&v).unwrap();
        assert!(!n.id.is_empty());
    }

    #[test]
    fn stub_harvest_session() {
        let tpl = sample_tpl(Accuracy {
            require_exploration_note: false,
            require_reason_on_every_edge: false,
            require_explicit_rejections: false,
            min_hard_negatives_per_batch: 0,
            min_criticals_per_batch: 0,
            ban_stub_notes: false,
            require_label_polarity: false,
        });
        let client = LlmClient::new("http://stub", "stub", None);
        let test_data = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../code_graph/examples/test_data");
        let source = Source::new(test_data, None);
        let reg = ToolRegistry::with_source(source.clone());
        let fat = Path::new("/tmp/test_fat_session.json");
        let _ = std::fs::remove_file(fat);
        run_harvest(&tpl, &client, &reg, &source, fat, None);
        let _ = std::fs::remove_file(fat);
    }
}
