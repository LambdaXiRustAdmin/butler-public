//! `butler_ask` façade routing — shared by stdio MCP and HTTP `/context`.
//!
//! Primary agent entry: no goal enum required. Maps symbol/query/mode → orchestrate goals.

use serde_json::{json, Value};

/// True when a free-form string looks like a symbol Ident (not prose).
pub fn looks_like_symbol_token(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.len() > 128 {
        return false;
    }
    if t.contains("::") {
        return t
            .split("::")
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
    t.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
        && !t.contains(' ')
}

/// Choose orchestrate goal from ask façade fields.
pub fn route_ask_goal(
    mode: &str,
    has_symbol: bool,
    has_scope: bool,
    has_file_line: bool,
    query_or_prompt: Option<&str>,
) -> &'static str {
    let mode = mode.trim().to_ascii_lowercase();
    let query_arch = query_or_prompt
        .map(|s| {
            let l = s.to_ascii_lowercase();
            l.contains("architect")
                || l.contains("overview")
                || l == "map"
                || l.contains("summary")
                || l.contains("structure")
        })
        .unwrap_or(false);

    match mode.as_str() {
        "trace" => "TraceBlastRadius",
        "find" => "FindImplementation",
        "arch" | "map" => "ArchitecturalSummary",
        _ if has_file_line && !has_symbol => "FindImplementation",
        _ if has_symbol => "TraceBlastRadius",
        _ if query_arch || (has_scope && !has_symbol) => "ArchitecturalSummary",
        _ if has_scope => "ArchitecturalSummary",
        _ => "TraceBlastRadius",
    }
}

/// Map façade JSON args → `/context` body (goal + symbol). Pure; unit-tested.
pub fn remap_butler_ask_args(mut arguments: Value) -> Value {
    let obj = match arguments.as_object_mut() {
        Some(o) => o,
        None => return arguments,
    };

    let symbol = obj
        .remove("symbol")
        .or_else(|| obj.remove("target_symbol"))
        .or_else(|| {
            obj.get("query")
                .cloned()
                .filter(|q| q.as_str().is_some_and(looks_like_symbol_token))
        });
    if let Some(sym) = symbol {
        obj.insert("target_symbol".to_string(), sym.clone());
        obj.insert("prompt".to_string(), sym);
    } else if let Some(q) = obj.get("query").cloned() {
        obj.insert("prompt".to_string(), q);
    }

    let mode = obj
        .remove("mode")
        .and_then(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
        .unwrap_or_else(|| "auto".into());

    let has_symbol = obj
        .get("target_symbol")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    let has_scope = obj
        .get("scope_paths")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_file_line = obj.get("target_file").is_some() && obj.get("target_line").is_some();
    let q = obj
        .get("query")
        .or_else(|| obj.get("prompt"))
        .and_then(|v| v.as_str());

    let goal = route_ask_goal(&mode, has_symbol, has_scope, has_file_line, q);
    // Dual-write: goal and mode must stay identical after façade routing so
    // mode_intent::intent_from_request (goal.or(mode)) cannot diverge.
    obj.insert("goal".to_string(), json!(goal));
    obj.insert("mode".to_string(), json!(goal));

    if !obj.contains_key("detail") {
        obj.insert("detail".to_string(), json!("compact"));
    }

    obj.insert(
        "mcp_tool_name".to_string(),
        json!("butler_orchestrate"),
    );
    arguments
}

/// Product MCP toolbelt: primary façade first (stable — no mid-session explosion).
///
/// Default (expert=false): `butler_ask`, `butler_orchestrate`, `butler_help`.
/// Expert: legacy search/inspect/map/context + project helpers + harvest schemas.
pub fn product_mcp_tools_json(
    expert: bool,
    harvest: Vec<Value>,
) -> Vec<Value> {
    use crate::butler_instructions::{
        BUTLER_ASK_TOOL_DESCRIPTION, BUTLER_HELP_TOOL_DESCRIPTION,
        BUTLER_ORCHESTRATE_TOOL_DESCRIPTION,
    };
    use crate::mcp_schema::{
        butler_ask_tool_schema, butler_context_tool_schema, butler_inspect_tool_schema,
        butler_map_tool_schema, butler_orchestrate_tool_schema, butler_search_tool_schema,
    };

    let mut tools = vec![
        json!({
            "name": "butler_ask",
            "description": BUTLER_ASK_TOOL_DESCRIPTION,
            "inputSchema": butler_ask_tool_schema()
        }),
        json!({
            "name": "butler_orchestrate",
            "description": BUTLER_ORCHESTRATE_TOOL_DESCRIPTION,
            "inputSchema": butler_orchestrate_tool_schema()
        }),
        json!({
            "name": "butler_help",
            "description": BUTLER_HELP_TOOL_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }),
    ];
    if expert {
        tools.extend([
            json!({
                "name": "butler_context",
                "description": "Legacy general-purpose context. Prefer `butler_ask`.",
                "inputSchema": butler_context_tool_schema()
            }),
            json!({
                "name": "butler_search",
                "description": "Symbol/keyword search. Prefer `butler_ask` with symbol=… for Trace.",
                "inputSchema": butler_search_tool_schema()
            }),
            json!({
                "name": "butler_inspect",
                "description": "Surgical file/line inspect. Prefer `butler_ask` with target_file+target_line.",
                "inputSchema": butler_inspect_tool_schema()
            }),
            json!({
                "name": "butler_map",
                "description": "Scoped structural map. Prefer `butler_ask` mode=arch + scope_paths.",
                "inputSchema": butler_map_tool_schema()
            }),
            json!({
                "name": "butler_list_projects",
                "description": "Lists available projects. Call first if you don't know project paths/names.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            }),
            json!({
                "name": "butler_select_project",
                "description": "Interactive project selector when switching codebases.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            }),
        ]);
        tools.extend(harvest);
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_default_tools_are_ask_first_stable() {
        let tools = product_mcp_tools_json(false, vec![]);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names, vec!["butler_ask", "butler_orchestrate", "butler_help"]);
    }

    #[test]
    fn product_expert_includes_ask_and_legacy() {
        let tools = product_mcp_tools_json(true, vec![json!({"name": "harvest_open"})]);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names[0], "butler_ask");
        assert!(names.contains(&"butler_search"));
        assert!(names.contains(&"harvest_open"));
    }

    #[test]
    fn ask_symbol_only_routes_to_trace() {
        let args = remap_butler_ask_args(json!({
            "project": "/projects/test_repos/redis",
            "symbol": "addReply"
        }));
        assert_eq!(args["goal"], "TraceBlastRadius");
        assert_eq!(args["target_symbol"], "addReply");
        assert_eq!(args["mcp_tool_name"], "butler_orchestrate");
    }
}
