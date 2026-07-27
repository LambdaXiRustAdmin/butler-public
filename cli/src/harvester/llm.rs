//! LLM connector via litellm (external Qwen/Grok).
//! Simple post, low temp, action parse. No ceremony.

use reqwest::blocking::Client;
use serde_json::{json, Value};

pub struct LlmClient {
    client: Client,
    base: String,
    model: String,
    api_key: Option<String>,
}

impl LlmClient {
    pub fn new(base: &str, model: &str, api_key: Option<&str>) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(
                    std::env::var("HARVESTER_LLM_TIMEOUT_SECS")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(900),
                )) // default 15 min, override via env for very slow local models
                .build()
                .expect("failed to create HTTP client"),
            base: base.to_string(),
            model: model.to_string(),
            api_key: api_key.map(|s| s.to_string()),
        }
    }

    /// `mailbox:/tmp/harvester_mbox` or `agent:mailbox:/path` → file protocol (no API $).
    pub fn mailbox_dir(&self) -> Option<std::path::PathBuf> {
        let b = self.base.trim();
        if let Some(rest) = b.strip_prefix("mailbox:") {
            return Some(std::path::PathBuf::from(rest));
        }
        if let Some(rest) = b.strip_prefix("agent:mailbox:") {
            return Some(std::path::PathBuf::from(rest));
        }
        None
    }

    pub fn ask(&self, prompt: &str) -> Option<Value> {
        if self.mailbox_dir().is_some() {
            // Mailbox handled by agent_loop/session, not HTTP ask.
            return None;
        }
        if self.base.contains("stub") {
            // Stub sequence for tests: first tool-ish call, then emit with hard negative.
            use std::sync::atomic::{AtomicU32, Ordering};
            static STUB_CALL: AtomicU32 = AtomicU32::new(0);
            let n = STUB_CALL.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 {
                return Some(json!({"action": "get_codegraph_nodes", "args": {}}));
            }
            return Some(json!({"action": "emit_batch", "args": {"nodes": [
                {"id": "rust_example.rs:function_item:5ea43c18", "node_type": "function_item", "name": "hello_rust", "file": "rust_example.rs", "range": "1-3", "snippet": "fn hello_rust() {}", "exploration_note": "core for query", "is_critical": true},
                {"id": "python_example.py:function_definition:213c22f8", "node_type": "function_definition", "name": "hello_python", "file": "python_example.py", "range": "1-2", "snippet": "def hello_python():", "exploration_note": "not relevant", "rejection_reason": "not on critical path for query"}
            ]}}));
        }
        // Real litellm: low temp, higher tokens, robust extraction
        eprintln!(
            "[harvester LLM] Calling {} with model={} (prompt len={})",
            self.base,
            self.model,
            prompt.len()
        );
        eprintln!(
            "[harvester LLM] Prompt preview (first 500 chars):\n{}",
            &prompt[..prompt.len().min(500)]
        );
        let _ = Self::append_live_log(&format!(
            "[harvester LLM] Calling {} with model={} (prompt len={})",
            self.base,
            self.model,
            prompt.len()
        ));
        // Log auth header presence (masked)
        if std::env::var("OPENAI_API_KEY").is_ok() || std::env::var("LITELLM_API_KEY").is_ok() {
            eprintln!("[harvester LLM] Using Authorization header with Bearer key (masked)");
        } else {
            eprintln!(
                "[harvester LLM] No API key env var found (OPENAI_API_KEY or LITELLM_API_KEY)"
            );
        }
        // To dump full prompt for inspection (uncomment if needed):
        // std::fs::write(format!("/tmp/harvester_prompt_{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()), &prompt).ok();
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.1,
            "max_tokens": 4000
        });
        let url = format!("{}/v1/chat/completions", self.base.trim_end_matches('/'));
        let mut req = self.client.post(&url).json(&body);
        // Use api_key from form if provided, else fall back to env (LITELLM first, then OPENAI)
        let key = self.api_key.clone().or_else(|| {
            std::env::var("LITELLM_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .ok()
        });
        if let Some(mut key) = key {
            if !key.trim().is_empty() {
                // Sanitize: remove any leading "Bearer " prefix if present
                let key_trim = key.trim();
                if key_trim.to_lowercase().starts_with("bearer ") {
                    key = key_trim[7..].trim().to_string();
                }
                let auth_header = format!("Bearer {}", key);
                eprintln!(
                    "[harvester LLM] Setting Authorization header: Bearer <masked-{}...>",
                    &key[..key.len().min(8)]
                );
                let _ = Self::append_live_log(&format!(
                    "[harvester LLM] Using auth key (masked first 8): {}",
                    &key[..key.len().min(8)]
                ));
                req = req.header("Authorization", auth_header);
            }
        } else {
            eprintln!("[harvester LLM] No Authorization header set (no key in form or env)");
            let _ = Self::append_live_log("[harvester LLM] No API key provided");
        }
        match req.send() {
            Ok(resp) => {
                let resp_text = resp.text().unwrap_or_default();
                eprintln!(
                    "[harvester LLM] Raw response text (first 800 chars):\n{}",
                    &resp_text[..resp_text.len().min(800)]
                );
                let _ =
                    Self::append_live_log(&format!("[harvester LLM] Raw response: {}", resp_text));
                // To dump full response:
                // std::fs::write(format!("/tmp/harvester_response_{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()), &resp_text).ok();

                if let Ok(v) = serde_json::from_str::<Value>(&resp_text) {
                    if let Some(err) = v.get("error") {
                        eprintln!("[harvester LLM] Error from LLM: {}", err);
                        let _ = Self::append_live_log(&format!("[harvester LLM] Error: {}", err));
                    }
                    // Surface output-budget kills (reasoning models often share max_tokens with CoT).
                    if let Some(fr) = v
                        .pointer("/choices/0/finish_reason")
                        .and_then(|x| x.as_str())
                    {
                        let usage = v.get("usage").cloned().unwrap_or(Value::Null);
                        eprintln!(
                            "[harvester LLM] finish_reason={fr} usage={usage}"
                        );
                        let _ = Self::append_live_log(&format!(
                            "[harvester LLM] finish_reason={fr} usage={usage}"
                        ));
                        if fr == "length" {
                            eprintln!(
                                "[harvester LLM] WARN: output hit max_tokens — incomplete emit likely (thinking may have eaten the budget)"
                            );
                            let _ = Self::append_live_log(
                                "[harvester LLM] WARN: finish_reason=length — incomplete emit likely",
                            );
                        }
                    }
                    let msg = &v["choices"][0]["message"];
                    let content = msg["content"].as_str().unwrap_or("");
                    let reasoning = msg["reasoning_content"].as_str().unwrap_or("");
                    let text = if !content.is_empty() {
                        content
                    } else {
                        reasoning
                    };
                    if let Some(action) = Self::extract_action(text) {
                        eprintln!("[harvester LLM] Got valid action");
                        return Some(action);
                    } else {
                        eprintln!("[harvester LLM] Could not extract JSON action from response (len={}, first 300 chars): {}", text.len(), &text[..text.len().min(300)]);
                    }
                } else {
                    eprintln!("[harvester LLM] Failed to parse response JSON");
                    let _ = Self::append_live_log("[harvester LLM] Failed to parse response JSON");
                }
            }
            Err(e) => {
                eprintln!("[harvester LLM] HTTP request to LLM failed: {}", e);
                let _ = Self::append_live_log(&format!(
                    "[harvester LLM] HTTP request to LLM failed: {}",
                    e
                ));
            }
        }
        None
    }

    fn append_live_log(line: &str) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/harvester_live.log")
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{}\n", line).as_bytes()));
    }

    /// Strip markdown code fences and extract first JSON object.
    fn extract_action(text: &str) -> Option<Value> {
        let mut s = text.trim();
        if let Some(start) = s.find("```") {
            let rest = &s[start + 3..];
            let rest = rest
                .trim_start_matches("json")
                .trim_start_matches('\n')
                .trim_start();
            if let Some(end) = rest.rfind("```") {
                s = rest[..end].trim();
            } else {
                s = rest.trim();
            }
        }
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            return Some(v);
        }
        // fallback: find balanced { ... }
        if let Some(start) = s.find('{') {
            if let Some(end) = s.rfind('}') {
                if let Ok(v) = serde_json::from_str::<Value>(&s[start..=end]) {
                    return Some(v);
                }
            }
        }
        None
    }
}
