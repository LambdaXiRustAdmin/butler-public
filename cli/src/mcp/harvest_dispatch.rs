//! Expert-only harvest tool surface for MCP (Mixed-three M2 peel).
//!
//! **Product belt default:** no harvest tools in `tools/list`, no harvest in day-to-day agent path.
//! **Expert / train:** `settings.agent.expert_mode` (or process flag) unlocks harvest_* schemas
//! and tools/call arms. Gold labeling only — not stranger Alpha map product.
//!
//! Zero intentional behavior change.

use serde_json::Value;

/// Harvest tool JSON schemas for `tools/list` when expert mode is on.
/// Empty when expert is false (product refuse path).
pub(crate) fn harvest_schemas_if_expert(expert: bool) -> Vec<Value> {
    if expert {
        crate::harvester::mcp_api::harvest_tool_schemas()
    } else {
        vec![]
    }
}

/// True when the tool name is a harvest_* gold-label call.
pub(crate) fn is_harvest_tool(name: &str) -> bool {
    name.starts_with("harvest_")
}

/// In-process gold labeling (same cards/gates as CLI harvester). Blocking graph load OK.
pub(crate) async fn dispatch_harvest_tool_call(name: &str, arguments: Value) -> Value {
    let n = name.to_string();
    tokio::task::spawn_blocking(move || {
        crate::harvester::mcp_api::dispatch_harvest_tool(&n, &arguments).unwrap_or_else(|| {
            serde_json::json!({
                "content": [{ "type": "text", "text": "Unknown harvest tool" }],
                "isError": true
            })
        })
    })
    .await
    .unwrap_or_else(|e| {
        serde_json::json!({
            "content": [{ "type": "text", "text": format!("harvest task failed: {e}") }],
            "isError": true
        })
    })
}
