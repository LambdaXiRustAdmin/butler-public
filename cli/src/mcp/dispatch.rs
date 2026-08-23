//! MCP product tools/list + tools/call dispatch (Mixed-three M1+M2 peel).
//!
//! Product routing lives here; expert harvest tools → [`super::harvest_dispatch`].
//! Stdio/HTTP transport stays in `mod.rs`.
//! Zero intentional behavior change.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::Value;

use super::handlers::handle_butler_context;
use super::harvest_dispatch::{
    dispatch_harvest_tool_call, harvest_schemas_if_expert, is_harvest_tool,
};
use crate::butler_ask::{product_mcp_tools_json, remap_butler_ask_args};
use crate::butler_instructions::BUTLER_ORCHESTRATE_INSTRUCTIONS;

/// Build the tools list for stdio "tools/list".
/// Stable product belt by default; expert_mode unlocks legacy + harvest schemas.
pub(crate) fn build_tools_list(expert: bool) -> Vec<Value> {
    let harvest = harvest_schemas_if_expert(expert);
    product_mcp_tools_json(expert, harvest)
}

/// Dispatch a tools/call after name/arguments extraction and remapping.
/// Product tool list is stable (no mid-session unlock) — list_changed not used for belt expansion.
pub(crate) async fn dispatch_tool_call(
    name: &str,
    mut arguments: Value,
    original_params: Option<&Value>,
    butler_url: &str,
    _expert_mode: &Arc<AtomicBool>,
) -> (Value, bool) {
    let should_send_list_changed = false;

    // Expert train path — owned by harvest_dispatch (not product map tools).
    if is_harvest_tool(name) {
        let result = dispatch_harvest_tool_call(name, arguments).await;
        return (result, should_send_list_changed);
    }

    let result = match name {
        "who_calls" | "butler_ask" => {
            // User-facing door is who_calls; butler_ask is the internal alias.
            let args = remap_butler_ask_args(arguments);
            handle_butler_context(args, butler_url).await
        }
        "butler_context" => handle_butler_context(arguments, butler_url).await,
        "butler_search" | "butler_inspect" | "butler_map" | "butler_orchestrate" => {
            // Remap for server-side ContextRequest compatibility (mcp_tool_name + aliases).
            if let Some(obj) = arguments.as_object_mut() {
                if name == "butler_search" {
                    if let Some(q) = obj.remove("query") {
                        obj.insert("prompt".to_string(), q);
                    }
                }
                if name == "butler_orchestrate" {
                    if let Some(g) = obj.remove("goal") {
                        obj.insert("goal".to_string(), g.clone());
                        obj.insert("mode".to_string(), g);
                    }
                    if let Some(t) = obj.remove("target_symbol") {
                        obj.insert("target_symbol".to_string(), t.clone());
                        obj.insert("prompt".to_string(), t);
                    }
                }
                obj.insert("mcp_tool_name".to_string(), serde_json::json!(name));
            }

            let _ = original_params; // reserved for diagnostics
            handle_butler_context(arguments, butler_url).await
        }
        "butler_help" => {
            let help_text = format!(
                "{}\n\n---\nCall 'butler_help' again anytime you are unsure.",
                BUTLER_ORCHESTRATE_INSTRUCTIONS
            );
            serde_json::json!({
                "content": [{ "type": "text", "text": help_text }],
                "isError": false
            })
        }
        "butler_list_projects" => {
            let projects_url = format!("{}/projects", butler_url);
            let projects_text = match crate::config::apply_client_auth(
                reqwest::Client::new()
                    .get(&projects_url)
                    .timeout(std::time::Duration::from_secs(10)),
            )
            .send()
            .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let body = resp.text().await.unwrap_or_default();
                    format!("Available projects:\n{}", body)
                }
                _ => "Could not list projects. The server may not be configured with BUTLER_PROJECTS_ROOT, or no projects were found.".to_string(),
            };
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("=== BUTLER LIST PROJECTS ===\n\n{}\n\nUse one of these names in the 'project' field when calling butler_context.", projects_text)
                }],
                "isError": false
            })
        }
        "butler_select_project" => {
            let projects_url = format!("{}/projects", butler_url);
            let projects_list = match crate::config::apply_client_auth(
                reqwest::Client::new()
                    .get(&projects_url)
                    .timeout(std::time::Duration::from_secs(8)),
            )
            .send()
            .await
            {
                Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
                _ => String::new(),
            };
            let projects_section = if projects_list.trim().is_empty() || projects_list == "[]" {
                "No projects were automatically discovered.\nYou can still specify any project name + optional root path below.".to_string()
            } else {
                format!("**Available Projects:**\n{}", projects_list)
            };
            let selection_prompt = format!(
                r#"=== BUTLER PROJECT SELECTION ===

Please choose which project (codebase) you want to analyze.

{}

**How to proceed:**

**A. Use a pre-discovered project** (from the list above or BUTLER_PROJECTS_ROOT):
```json
{{
  "project": "project-name-here",
  "prompt": "your keywords"
}}
```

**B. Analyze ANY external repository on disk (recommended for fd, bat, bevy, etc.):**
Pass the **full absolute path** directly in the `project` field (or `root`):
```json
{{
  "project": "/projects/test_repos/fd",
  "prompt": "main function or cli entry point",
  "mode": "balanced"
}}
```
or
```json
{{
  "project": "bevy",
  "root": "/projects/test_repos/bevy",
  "prompt": "ecs system"
}}
```

Butler supports on-demand scanning of any Rust or Python directory via absolute paths — no pre-indexing required.

Call `butler_select_project` again anytime you want to switch or discover more."#,
                projects_section
            );
            serde_json::json!({
                "content": [{ "type": "text", "text": selection_prompt }],
                "isError": false
            })
        }
        _ => serde_json::json!({
            "content": [{ "type": "text", "text": format!("Unknown tool: '{}'. Supported tools: 'butler_context', 'butler_help', 'butler_list_projects', harvest_*.", name) }],
            "isError": true
        }),
    };

    (result, should_send_list_changed)
}
