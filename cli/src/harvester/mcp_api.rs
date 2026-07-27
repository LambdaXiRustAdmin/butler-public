//! MCP / JSON-RPC facing harvest API — thin wrappers over `session`.
//! Accurate graph→labeler transfer: cards only, same gates as litellm path.

use super::session::{
    global_close, global_open, global_status, global_with, template_from_open_args, HarvestSession,
};
use super::source::Source;
use serde_json::{json, Value};
use std::path::PathBuf;

fn mcp_ok(v: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()) }],
        "structuredContent": v,
        "isError": false
    })
}

fn mcp_err(msg: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

/// harvest_open — start session (load graph, fat).
pub fn harvest_open(args: &Value) -> Value {
    let repo = args
        .get("repo")
        .or_else(|| args.get("project"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if repo.is_empty() {
        return mcp_err("repo/project required (absolute path to codebase)");
    }
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("main entry and core types");
    let fat = args
        .get("fat_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(repo).join(".butler/fat.json"));
    let export = args
        .get("butler_export")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let scope: Vec<String> = args
        .get("scope_paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_else(|| vec!["src".into()]);
    let card_profile = args
        .get("card_profile")
        .and_then(|v| v.as_str())
        .unwrap_or("fast");
    let batch_size = args
        .get("batch_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let max_steps = args
        .get("max_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(40) as usize;
    let target_criticals = args
        .get("target_criticals")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let target_rejections = args
        .get("target_rejections")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("agent");

    let tpl = template_from_open_args(
        repo,
        query,
        model,
        batch_size,
        max_steps,
        target_criticals,
        target_rejections,
        scope,
        export.as_ref().map(|p| p.to_string_lossy().into_owned()),
        card_profile,
    );
    let source = Source::new(PathBuf::from(repo), export);
    let session = HarvestSession::open(tpl, source, fat, true);
    let status = session.status();
    global_open(session);
    mcp_ok(json!({
        "ok": true,
        "message": "harvest session open — call harvest_next_cards then harvest_commit",
        "card_profile": card_profile,
        "status": status,
    }))
}

pub fn harvest_next_cards(_args: &Value) -> Value {
    match global_with(|s| s.next_card_batch()) {
        Ok(Ok(batch)) => mcp_ok(json!({
            "ok": true,
            "protocol": "butler_harvest_v1",
            "batch": batch,
            "instruction": "Label ONLY ids from batch.cards. Return harvest_commit with nodes array (is_critical or rejection_reason + exploration_note)."
        })),
        Ok(Err(e)) => mcp_err(&e),
        Err(e) => mcp_err(&e),
    }
}

pub fn harvest_commit(args: &Value) -> Value {
    let nodes = args
        .get("nodes")
        .or_else(|| args.pointer("/args/nodes"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match global_with(|s| s.commit_emit(&nodes)) {
        Ok(r) => {
            if r.ok {
                mcp_ok(json!({ "ok": true, "result": r }))
            } else {
                json!({
                    "content": [{ "type": "text", "text": format!("commit failed: {:?}", r.issues) }],
                    "structuredContent": r,
                    "isError": true
                })
            }
        }
        Err(e) => mcp_err(&e),
    }
}

pub fn harvest_status(_args: &Value) -> Value {
    match global_status() {
        Ok(s) => mcp_ok(json!({ "ok": true, "status": s })),
        Err(e) => mcp_err(&e),
    }
}

pub fn harvest_tool(args: &Value) -> Value {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let tool_args = args.get("args").cloned().unwrap_or(json!({}));
    if action.is_empty() {
        return mcp_err("action required (read_file|grep|get_neighborhood_card)");
    }
    match global_with(|s| s.tool(action, &tool_args)) {
        Ok(v) => mcp_ok(v),
        Err(e) => mcp_err(&e),
    }
}

pub fn harvest_close(_args: &Value) -> Value {
    match global_close() {
        Some(s) => mcp_ok(json!({ "ok": true, "final": s })),
        None => mcp_err("no session to close"),
    }
}

pub fn harvest_tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "harvest_open",
            "description": "Start gold-label harvest session. Butler builds neighborhood cards from CodeGraph; you only stamp critical/reject. Use card_profile=fast (agent/API) or slow (local CPU).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Absolute path to codebase" },
                    "project": { "type": "string", "description": "Alias for repo" },
                    "query": { "type": "string" },
                    "fat_path": { "type": "string" },
                    "butler_export": { "type": "string" },
                    "scope_paths": { "type": "array", "items": { "type": "string" } },
                    "card_profile": { "type": "string", "enum": ["fast", "slow"], "description": "fast=large cards; slow=compact for local models" },
                    "batch_size": { "type": "integer" },
                    "max_steps": { "type": "integer" },
                    "target_criticals": { "type": "integer" },
                    "target_rejections": { "type": "integer" },
                    "model": { "type": "string" }
                },
                "required": ["repo"]
            }
        }),
        json!({
            "name": "harvest_next_cards",
            "description": "Get next neighborhood card batch (accurate graph→labeler transfer). Label only these ids.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }),
        json!({
            "name": "harvest_commit",
            "description": "Commit emit_batch nodes (fail-closed gates + catch-up caps). Same validation as litellm harvester.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "nodes": {
                        "type": "array",
                        "description": "FatNode-like objects with id, exploration_note, is_critical and/or rejection_reason"
                    }
                },
                "required": ["nodes"]
            }
        }),
        json!({
            "name": "harvest_status",
            "description": "Current fat counts, targets, goals_met.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }),
        json!({
            "name": "harvest_tool",
            "description": "Optional repo tool under harvest root: read_file, grep, get_neighborhood_card. Prefer labeling from cards.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "args": { "type": "object" }
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "harvest_close",
            "description": "Finalize fat (dedup) and close session.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }),
    ]
}

pub fn dispatch_harvest_tool(name: &str, args: &Value) -> Option<Value> {
    match name {
        "harvest_open" => Some(harvest_open(args)),
        "harvest_next_cards" => Some(harvest_next_cards(args)),
        "harvest_commit" => Some(harvest_commit(args)),
        "harvest_status" => Some(harvest_status(args)),
        "harvest_tool" => Some(harvest_tool(args)),
        "harvest_close" => Some(harvest_close(args)),
        _ => None,
    }
}


#[cfg(test)]
mod smoke {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_next_commit_on_test_data() {
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../code_graph/examples/test_data");
        let open = harvest_open(&json!({
            "repo": repo.to_string_lossy(),
            "query": "polyglot hello entry points",
            "fat_path": "/tmp/mcp_smoke_fat.json",
            "card_profile": "fast",
            "batch_size": 2,
            "max_steps": 3,
            "target_criticals": 1,
            "target_rejections": 1,
            "scope_paths": []
        }));
        assert_eq!(open.get("isError"), Some(&json!(false)), "{open}");
        let next = harvest_next_cards(&json!({}));
        assert_eq!(next.get("isError"), Some(&json!(false)), "{next}");
        let cards = next["structuredContent"]["batch"]["cards"].as_array().unwrap();
        assert!(!cards.is_empty());
        let mut nodes = vec![json!({
            "id": cards[0]["center_id"],
            "name": cards[0]["center_name"],
            "node_type": cards[0]["kind"],
            "file": cards[0]["file"],
            "range": cards[0]["range"],
            "snippet": cards[0]["snippet"],
            "exploration_note": "core symbol for the polyglot hello entry path in this mini repo",
            "is_critical": true
        })];
        if cards.len() > 1 {
            nodes.push(json!({
                "id": cards[1]["center_id"],
                "name": cards[1]["center_name"],
                "node_type": cards[1]["kind"],
                "file": cards[1]["file"],
                "range": cards[1]["range"],
                "snippet": cards[1]["snippet"],
                "exploration_note": "secondary symbol not required for the hello entry query",
                "is_critical": false,
                "rejection_reason": "not on the primary hello entry path for this query"
            }));
        }
        let commit = harvest_commit(&json!({ "nodes": nodes }));
        eprintln!("{}", serde_json::to_string_pretty(&commit).unwrap());
        assert_eq!(commit.get("isError"), Some(&json!(false)), "{commit}");
        let _ = harvest_close(&json!({}));
        let _ = std::fs::remove_file("/tmp/mcp_smoke_fat.json");
    }
}
