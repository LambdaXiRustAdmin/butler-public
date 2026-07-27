# Butler Tools — Usage Guide (Beta)

**For local models: Start here.**

**PRIMARY AND ONLY TOOL:** Use `butler_orchestrate` for ALL exploration, traces, architectural summaries, and implementation finds.

`butler_context` is DEPRECATED and should not be used. `butler_orchestrate` is the primary and only tool the LLM should use for exploration, traces, and architectural summaries.

Butler Orchestrate returns a pure JSON object. You must parse the JSON directly to read the `target` definition, `callers`, `callees`, `skeleton` paths, and `hubs`.

---

## Recommended Tool: `butler_orchestrate`

This is the primary entry point for smaller models.

### Goals

- **TraceBlastRadius**: Trace the impact (usages, callers, callees) of a specific symbol or struct.
- **ArchitecturalSummary**: Get a high-level structural overview of a scope (uses Semantic Zoom automatically).
- **FindImplementation**: Get a deep implementation view of a symbol.

### Parameters

- `goal` (required): One of the three goals above.
- `target_symbol`: The symbol to analyze (required for TraceBlastRadius and FindImplementation).
- `scope_paths`: List of directories to restrict analysis to (highly recommended).
- `ignore_paths`: Directories to exclude.

### Examples

**Architectural overview of a module:**
```json
{
  "project": "/projects/test_repos/fd",
  "goal": "ArchitecturalSummary",
  "scope_paths": ["src/"]
}
```

**Trace the blast radius of a function:**
```json
{
  "project": "/projects/test_repos/fd",
  "goal": "TraceBlastRadius",
  "target_symbol": "build_pattern_regex",
  "scope_paths": ["src/"]
}
```

**Find implementation details:**
```json
{
  "project": "/projects/test_repos/fd",
  "goal": "FindImplementation",
  "target_symbol": "ensure_use_hidden_option_for_leading_dot_pattern",
  "scope_paths": ["src/"]
}
```

**Self-healing behavior**: If no results are found for a step, the report will clearly explain what was tried and suggest next actions.

---

## Other Available Tools (for power users)

- `butler_search`: Keyword/symbol search or quick skeleton.
- `butler_map`: Structural skeleton of a scope (with Semantic Zoom).
- `butler_inspect`: Precise surgical or implementation inspection of a specific location/symbol.
- `butler_context`: Legacy general-purpose tool (still works, but prefer the tools above for new usage).

For complex refactoring or broad exploration, **strongly prefer `butler_orchestrate`**.

---

## Working Set Scoping (Important)

All modern tools support `scope_paths` and `ignore_paths`.

**Best practice**: Always provide `scope_paths` when you know which part of the code matters. This dramatically improves relevance and reduces token usage.

Example:
```json
"scope_paths": ["src/", "crates/core/"]
```

---

**Remember**: Butler is designed for focused, high-signal context. Use scoping and choose the right goal/tool.

## Real-world usage notes (from direct MCP agent experience)

When calling via the MCP `butler__butler_orchestrate` tool (the name exposed to agents):

- You receive `content[0].text` as a **short compact trace**, e.g.:
  `Trace for pyclass: 0 callers, 0 callees (1 highly relevant blocks).`
  or `Trace for FromPyObject: 14 callers, 5 callees (20 highly relevant blocks).`

- **Always** read the `structuredContent` (or `structured` over HTTP) for the actual data:
  - `hubs` (highly connected nodes)
  - `callers` / `callees`
  - `skeleton` (file paths)
  - `target` (definition snippet)
  - `telemetry` (pruning info, fan-out, visited counts)
  - `state` (e.g. cache status, "Edge Build: 100% | Cached")

- The "N highly relevant blocks" number comes from the neural/GNN scoring step (Butler writes a prompt-specific subgraph export → calls eve `--gnn-score-context` → applies scores + diversity penalty). The eve side now uses a transparent CPU reference forward for inference (much easier to debug than the full training tape).

- For "how does X work" / implementation questions, prefer:
  - `goal`: "FindImplementation"
  - `target_symbol`: the core concept or symbol (e.g. "pyclass", "FromPyObject", "PyClassPyO3Options"). It does **not** need to be an exact Rust identifier.
  - `scope_paths`: **always** provide (e.g. `["src/", "pyo3-macros-backend/src/"]`). Without it results are too broad, slow, and hit caps.

- `project`: absolute path to the root containing `Cargo.toml` (or `pyproject.toml`). It self-heals upward a few levels and auto-injects the path into scope_paths.

- First call on a fresh repo can trigger graph building. The `content` may start with `=== Building Graph ===`. MCP bridges usually auto-retry; you may see a short "building" trace on the first call.

- `butler_orchestrate` gives **analysis + pointers** (traces, hubs, callers). The pretty formatted context with actual code snippets (e.g. `=== Butler Context (Balanced) ===` + source blocks) is produced by the context assembly layer (used by the `butler context` CLI command and the dashboard at http://localhost:8002). It consumes the scored hubs from the same pipeline.

- Direct `eve --gnn-score-context` (what butler calls under the hood for neural) returns the **full** `{ "node_id": score }` map for whatever subgraph was exported. This is internal data for butler — not meant for direct human consumption. You will see the prompt boost mixed with the structural GNN component (now visible and non-zero via the CPU path).

- Check `structured.telemetry` on big results. If you see high `fan_out_pruned` or `visited_capped`, tighten `scope_paths`.

Recommended agent flow:
1. Call with `ArchitecturalSummary` or `FindImplementation` + tight `scope_paths` + `target_symbol`.
2. Look at `structuredContent.hubs`, `callers`, and the "highly relevant blocks" count.
3. Follow up with more targeted `FindImplementation` calls on the interesting symbols.
4. For the final context fed to the LLM, the assembly step (or running the `butler context "your prompt"` CLI) will turn the analysis into clean, token-efficient code blocks.

This reduces guessing about response shape, what "highly relevant" means, and when to use scope_paths vs. raw broad queries.

---

## Configuration

Butler supports a layered configuration system (lowest to highest priority):

1. Hardcoded defaults in the binary.
2. Global config file (`~/.config/butler/config.toml` or platform equivalent via `directories`).
3. Workspace config (`.butler/config.toml` in the current directory).
4. Environment variables using the `BUTLER__` prefix with `__` as separator (e.g. `BUTLER__SERVER__PORT=9000`, `BUTLER__AGENT__EXPERT_MODE=true`).

Example `.butler/config.toml`:

```toml
[server]
port = 9000
host = "0.0.0.0"

[analysis]
worker_stack_size_mb = 64
skip_directories = ["target", "node_modules", ".git", "dist", "build", "vendor"]

[agent]
expert_mode = true
default_max_tokens = 3000
```

When `agent.expert_mode = true`, the orchestrator is considered "already used" so advanced users aren't nagged.

All settings are loaded once at server startup with clear warnings (never panics) if a config file is malformed.
