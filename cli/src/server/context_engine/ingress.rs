//! Ingress / normalize (P3.1 stage peel S0).
//!
//! Caps, butler_ask normalize, path sanitize, help/meta/location early exits,
//! effective prompt, hallucination guard on target_line, NL guidance detect.
//!
//! Zero intentional behavior change.

use std::time::Instant;

use axum::{http::StatusCode, Json};
use code_graph::snooper::normalize_path;

use crate::server::analysis::{analyze_butler_prompt_intent, detect_natural_language_guidance};
use crate::server::dto::*;
use cli::butler_instructions::BUTLER_ORCHESTRATE_INSTRUCTIONS;

use super::resolve::normalize_butler_ask_request;

/// Values produced by successful ingress (no early exit).
pub(super) struct IngressReady {
    pub force_surgical: bool,
    pub effective_prompt: String,
    pub nl_guidance: Option<String>,
}

pub(super) enum IngressOutcome {
    Early(Result<(StatusCode, Json<ContextResponse>), String>),
    Ready(IngressReady),
}

/// Prompt for selection, neural subgraph, audit logs, and request telemetry.
/// When `prompt` is empty (common for `butler_orchestrate` HTTP calls), falls back to `target_symbol`.
pub fn effective_prompt_for_request(req: &ContextRequest) -> String {
    coalesce_prompt_from_target_symbol(req, raw_prompt_from_request(req))
}

fn raw_prompt_from_request(req: &ContextRequest) -> String {
    if req.target_file.is_some() && req.target_line.is_some() {
        return req.prompt.clone();
    }
    match analyze_butler_prompt_intent(&req.prompt) {
        PromptIntent::NormalSearch { cleaned_prompt } => cleaned_prompt,
        PromptIntent::MetaQuestion | PromptIntent::LocationTargeting => req.prompt.clone(),
    }
}

pub(super) fn coalesce_prompt_from_target_symbol(req: &ContextRequest, prompt: String) -> String {
    if !prompt.trim().is_empty() {
        return prompt;
    }
    req.target_symbol
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(prompt)
}

/// Normalize caps + paths + prompt intent. Early-exit help / meta / location guidance.
pub(super) fn run_ingress(req: &mut ContextRequest, overall_start: Instant) -> IngressOutcome {
    req.max_tokens = req.max_tokens.min(4000);
    if req.max_results == 0 {
        req.max_results = 8;
    }

    // HTTP MCP: `butler_ask` façade → orchestrate goals (same remap as stdio bridge).
    normalize_butler_ask_request(req);

    // === Sanitize all ingress paths for cross-platform (Windows \ -> /) consistency ===
    // This is the MCP/LLM input boundary. Must happen before any graph/root resolution or matching.
    if let Some(p) = &mut req.project {
        *p = normalize_path(p);
    }
    req.root = normalize_path(&req.root);
    if let Some(v) = &mut req.scope_paths {
        for s in v.iter_mut() {
            *s = normalize_path(s);
        }
    }
    if let Some(t) = &mut req.target_file {
        *t = normalize_path(t);
    }

    let lower = req.prompt.to_lowercase();

    // Help path (uses single const)
    if lower == "help"
        || lower == "instructions"
        || lower == "usage"
        || lower == "how to use"
        || lower.contains("how to use butler")
        || lower.contains("butler_context")
        || lower.contains("usage instructions")
        || lower.contains("tool instructions")
    {
        return IngressOutcome::Early(Ok((
            StatusCode::OK,
            Json(crate::server::filters::degenerate_context_response(
                BUTLER_ORCHESTRATE_INSTRUCTIONS.to_string(),
                Some("usage instructions".to_string()),
                Some("instructions".to_string()),
                0,
                false,
                overall_start.elapsed().as_millis() as u64,
            )),
        )));
    }

    let mut force_surgical = req.target_file.is_some() && req.target_line.is_some();

    let (mut effective_prompt, _is_meta) = if force_surgical {
        (
            coalesce_prompt_from_target_symbol(req, req.prompt.clone()),
            false,
        )
    } else {
        match analyze_butler_prompt_intent(&req.prompt) {
            PromptIntent::MetaQuestion => {
                let direct_header = "[Butler Instructions]\n\nThis was a request for information about the tool.\n\nCall `butler_help` with no arguments to receive the usage guide.\n\nCall `butler_select_project` with no arguments to list available projects and select one.\n\nThe `project` parameter is required on every call to `butler_orchestrate`.\n\nBelow are the official instructions:";
                let full_response =
                    format!("{}\n\n{}", direct_header, BUTLER_ORCHESTRATE_INSTRUCTIONS);
                return IngressOutcome::Early(Ok((
                    StatusCode::OK,
                    Json(crate::server::filters::degenerate_context_response(
                        full_response,
                        Some("meta question - full instructions served".to_string()),
                        Some("help_file_served".to_string()),
                        0,
                        false,
                        overall_start.elapsed().as_millis() as u64,
                    )),
                )));
            }
            PromptIntent::LocationTargeting => {
                if req.target_file.is_some() && req.target_line.is_some() {
                    force_surgical = true;
                    (
                        coalesce_prompt_from_target_symbol(req, req.prompt.clone()),
                        false,
                    )
                } else {
                    let redirect = r#"[Butler Guidance - Specific Location Detected]

You mentioned a specific line in a module (e.g. "line 17 of fused_chain"), but did not provide the required parameters.

**Correct way to query a specific line:**

```json
{
  "target_file": "platform-native/src/fused_chain.rs",
  "target_line": 17,
  "mode": "surgical",
  "depth": 1
}
```

The tool will then return the exact source at that line plus its direct callers and callees.

Please call `butler_context` again with `target_file` and `target_line`."#;
                    return IngressOutcome::Early(Ok((
                        StatusCode::OK,
                        Json(crate::server::filters::degenerate_context_response(
                            redirect.to_string(),
                            Some("location targeting - use surgical mode".to_string()),
                            Some("location_guidance".to_string()),
                            0,
                            false,
                            overall_start.elapsed().as_millis() as u64,
                        )),
                    )));
                }
            }
            PromptIntent::NormalSearch { cleaned_prompt } => (
                coalesce_prompt_from_target_symbol(req, cleaned_prompt),
                false,
            ),
        }
    };

    if effective_prompt.trim().is_empty() {
        effective_prompt = effective_prompt_for_request(req);
    }

    // Hallucination guard (light clones only on paths)
    if req.target_line.is_some() && !req.prompt.trim().is_empty() {
        let line = req.target_line.unwrap();
        let looks_hallucinated = !req.prompt.contains(' ') || line == 0 || line > 1_000_000;
        if looks_hallucinated {
            req.target_line = None;
            req.target_file = None;
            if req.mode.is_none() && line != 0 {
                req.mode = Some("mini".to_string());
            }
            force_surgical = false;
        }
    }
    if let Some(line) = req.target_line {
        if line > 1_000_000 {
            eprintln!(
                "[Butler] Rejecting absurd target_line: {:?}",
                req.target_line
            );
            req.target_line = None;
            req.target_file = None;
            if req.mode.is_none() {
                req.mode = Some("balanced".to_string());
            }
            force_surgical = false;
        }
    }

    let nl_guidance = detect_natural_language_guidance(&effective_prompt);

    IngressOutcome::Ready(IngressReady {
        force_surgical,
        effective_prompt,
        nl_guidance,
    })
}
