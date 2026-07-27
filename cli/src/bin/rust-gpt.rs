//! Smart front-end CLI for submitting natural language prompts to the Butler server (via /context as butler_orchestrate).

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::error::Error;

#[derive(Parser)]
#[command(author, version, about = "Rust GPT — smart front-end for Butler")]
struct Args {
    /// Natural language prompt (e.g. "analyze GPU executor memory allocation")
    prompt: String,

    /// Short project name (e.g. "my-project", "lambda-xi-rust")
    #[arg(short, long)]
    project: Option<String>,

    /// Context depth (default 3)
    #[arg(short, long, default_value_t = 3)]
    depth: usize,

    /// Max tokens (default 4000)
    #[arg(short, long, default_value_t = 4000)]
    max_tokens: usize,

    /// Butler server URL (default matches your docker-compose host port)
    #[arg(short, long, default_value = "http://localhost:8002")]
    url: String,
}

#[derive(Serialize)]
struct ButlerRequest {
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    depth: usize,
    max_tokens: usize,
    compress_tests: bool,
}

#[derive(Deserialize)]
struct ButlerResponse {
    content: String,
    selected_count: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let optimized_prompt = optimize_prompt(&args.prompt);
    let client = reqwest::Client::new();

    let req = ButlerRequest {
        prompt: optimized_prompt,
        project: args.project,
        depth: args.depth,
        max_tokens: args.max_tokens,
        compress_tests: true,
    };

    let url = format!("{}/context", args.url.trim_end_matches('/'));
    println!("→ Calling Butler at {}", url);

    let response = client.post(&url).json(&req).send().await?;
    let status = response.status();
    let bytes = response.bytes().await?;

    if status.is_success() {
        match serde_json::from_slice::<ButlerResponse>(&bytes) {
            Ok(resp) => {
                println!("{}", resp.content);
                println!("\n---\nSelected {} blocks", resp.selected_count);
            }
            Err(_) => {
                let text = String::from_utf8_lossy(&bytes);
                println!("{}", text);
            }
        }
    } else {
        let text = String::from_utf8_lossy(&bytes);
        eprintln!("❌ Server error ({}): {}", status, text);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Language-generic prompt optimizer (works for Rust, Python, any language)
// Keeps intent + universal code-analysis terms only
// ─────────────────────────────────────────────────────────────
fn optimize_prompt(raw: &str) -> String {
    let lower = raw.to_lowercase();

    // Universal keep-words (no Rust/Python specific terms)
    let _keep_words = [
        "output",
        "format",
        "parse",
        "generate",
        "llm",
        "analyze",
        "structure",
        "function",
        "method",
        "class",
        "module",
        "import",
        "export",
        "api",
        "config",
        "test",
        "graph",
        "edge",
        "call",
        "called_by",
        "data",
        "flow",
    ];

    let mut keywords: Vec<&str> = lower.split_whitespace().filter(|w| w.len() > 2).collect();

    // Light query expansion for common code-analysis patterns (language-agnostic)
    if lower.contains("output")
        || lower.contains("format")
        || lower.contains("parse")
        || lower.contains("generate")
    {
        keywords.extend_from_slice(&[
            "function", "method", "module", "api", "config", "data", "flow",
        ]);
    }
    if lower.contains("llm") || lower.contains("analyze") || lower.contains("structure") {
        keywords.extend_from_slice(&["graph", "edge", "call", "called_by", "test"]);
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    keywords.retain(|&w| seen.insert(w));

    if keywords.is_empty() {
        raw.to_string()
    } else {
        keywords.join(" ")
    }
}
