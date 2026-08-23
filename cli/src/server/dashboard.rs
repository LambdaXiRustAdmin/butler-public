//! Operator lab HTML (`GET /` and `GET /ops`).
//!
//! Full orchestrate / export / harvester. **Not** first-run install —
//! that is [`super::setup_page::render_setup`] at `/setup`.

use axum::response::Html;

pub async fn render_dashboard() -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Butler Operator Tools</title>
    <style>
        :root { --bg: #0d1117; --fg: #c9d1d9; --card: #161b22; --border: #30363d; --accent: #238636; }
        body { font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: var(--bg); color: var(--fg); margin: 0; padding: 20px; line-height: 1.5; }
        .container { max-width: 960px; margin: 0 auto; }
        header { border-bottom: 1px solid var(--border); padding-bottom: 12px; margin-bottom: 20px; }
        h1 { margin: 0 0 4px; font-size: 1.6rem; }
        .status { font-size: 0.85rem; color: #8b949e; }
        .card { background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: 16px; margin-bottom: 16px; }
        label { display: block; font-size: 0.8rem; margin-bottom: 4px; color: #8b949e; }
        input, select, textarea { width: 100%; box-sizing: border-box; background: #0d1117; color: var(--fg); border: 1px solid var(--border); padding: 8px 10px; border-radius: 4px; font-family: inherit; font-size: 0.95rem; }
        textarea { min-height: 60px; resize: vertical; font-family: ui-monospace, monospace; }
        .row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
        button { background: var(--accent); color: white; border: none; padding: 10px 16px; border-radius: 4px; font-size: 0.95rem; cursor: pointer; margin-top: 8px; }
        button:hover { filter: brightness(1.1); }
        #output { background: #0d1117; border: 1px solid var(--border); padding: 12px; border-radius: 4px; min-height: 120px; white-space: pre-wrap; font-family: ui-monospace, monospace; font-size: 0.8rem; overflow: auto; max-height: 420px; }
        .hint { font-size: 0.75rem; color: #8b949e; }
        .banner { background: #1c2128; border: 1px solid #388bfd; border-radius: 6px; padding: 12px 14px; margin-bottom: 16px; font-size: 0.9rem; }
        .banner a { color: #58a6ff; }
    </style>
</head>
<body>
<div class="container">
    <div class="banner">
        <b>Operator lab</b> — export, training, harvester, full orchestrate.
        First-run / MCP proof of life: <a href="/setup">/setup</a> (not this page).
    </div>
    <header>
        <h1>Butler Operator Tools</h1>
        <div class="status">
            Port 8002 • Version 0.1 • Built: <span id="build-time">just now</span>
            · <a href="/setup" style="color:#58a6ff">← Setup / install</a>
            · <a href="https://ko-fi.com/lambdawisperer" target="_blank" rel="noopener noreferrer" style="color:#8b949e">Support</a>
        </div>
    </header>

    <div class="card">
        <h3 style="margin-top:0">Setup notes (operator)</h3>
        <p class="hint" style="margin-top:0">Docker is optional. Strangers should use <a href="/setup" style="color:#58a6ff">/setup</a> first.</p>
        <ol style="margin:0 0 8px 1.2em; font-size:0.9rem;">
            <li><b>Where does Butler live?</b> Binaries via <code>./install.sh</code> or <code>cargo build --release -p cli</code> (this tree / <code>~/.local</code>).</li>
            <li><b>Where are your program files?</b> Absolute project roots — put them in <b>Project path</b> below (agents pass the same as <code>project</code>).</li>
            <li><b>Where is the server?</b>
                <ul style="margin:4px 0 0 0;">
                    <li><b>Local native</b> — <code>butler-server</code> on this machine (default <code>127.0.0.1:8002</code>)</li>
                    <li><b>Remote</b> — same binary elsewhere; clients set <code>BUTLER_URL</code></li>
                    <li><b>Docker</b> — optional; map host code into the container and use container paths</li>
                </ul>
            </li>
        </ol>
        <p class="hint" style="margin:0">Full Alpha one-pager: <code>plans/ALPHA_SETUP.md</code> in the repo. Agents: MCP <code>who_calls</code> → this server URL — not this HTML.</p>
        <p class="hint" style="margin:6px 0 0">
            Health: <code>GET /mcp/health</code> · Config: <code>~/.config/butler/config.toml</code> or <code>.butler/config.toml</code><br>
            <b>Username</b> defaults to this computer’s hostname (<code>server.username</code> / <code>BUTLER_USERNAME</code>).
            <b>Optional password</b> (<code>server.password</code> / <code>BUTLER_PASSWORD</code>) — when set, API needs
            <code>Authorization: Bearer …</code>. Prefer <code>host = "127.0.0.1"</code> if password is empty.
        </p>
    </div>

    <div class="card">
        <h3 style="margin-top:0">Submit butler_orchestrate</h3>
        <form id="orch-form" onsubmit="submitOrchestrate(event)">
            <div class="row">
                <div>
                    <label for="project">Project path (absolute or relative)</label>
                    <input id="project" type="text" value="/home/you/projects/my-app" placeholder="/absolute/path/to/repo" required>
                </div>
                <div>
                    <label for="goal">Goal</label>
                    <select id="goal">
                        <option value="ArchitecturalSummary">ArchitecturalSummary</option>
                        <option value="TraceBlastRadius">TraceBlastRadius</option>
                        <option value="FindImplementation">FindImplementation</option>
                    </select>
                </div>
            </div>
            <div>
                <label for="scope">scope_paths (comma-separated, e.g. src,crates/core or leave empty)</label>
                <input id="scope" type="text" placeholder="src">
            </div>
            <div>
                <label for="ignore">ignore_paths (comma-separated, e.g. tests,benchmarks,examples,docs) — strongly recommended for clean exports</label>
                <input id="ignore" type="text" placeholder="tests,benchmarks,examples,docs,tools">
            </div>
            <div>
                <label for="target">target_symbol (only for Trace/Find)</label>
                <input id="target" type="text" placeholder="optional">
            </div>
            <div>
                <label for="detail">detail (length mode)</label>
                <select id="detail">
                    <option value="short" selected>short (compact) — tight sample / orient</option>
                    <option value="long">long (dense) — larger sample under pin</option>
                </select>
                <div class="hint" style="font-size:0.7rem">Prefer short first; re-submit long with same scope if the sample is thin. Honesty (degrees/omitted) is the same either way.</div>
            </div>
            <button type="submit">Submit (POST /context as butler_orchestrate)</button>
            <button type="button" onclick="exportButlerGraph()" style="margin-left: 8px;">Export Butler Graph (skinny export)</button>
        </form>
        <div class="hint">Response will appear below. The server returns the exact JSON shape used by the MCP bridge.</div>
        <div class="hint">The Export button forces a fresh graph_export.json (both cache and dataset/) **respecting scope_paths + ignore_paths from the form above** (filtered export for clean keepers/GNN data). Use this to refresh the "skinny" export.</div>
    </div>

    <div class="card">
        <h3 style="margin-top:0">Build Training Bundle (structural graph for Eve GNN)</h3>
        <form id="bundle-form" onsubmit="submitTrainingBundle(event)">
            <div class="row">
                <div>
                    <label>Repo path</label>
                    <input id="bundle-repo" type="text" value="/projects/test_repos/bat">
                </div>
                <div>
                    <label>Output dir (relative to repo)</label>
                    <input id="bundle-out" type="text" value=".butler/training">
                </div>
            </div>
            <button type="submit">Build Bundle (graph_export.json only)</button>
        </form>
        <div id="bundle-status" style="margin-top:8px; white-space:pre-wrap; font-family:monospace; font-size:0.8rem; background:#0d1117; border:1px solid #30363d; padding:8px; min-height:40px;"></div>
        <div class="hint">Forces heavy parse (rich nodes + edges + containment). Writes &lt;repo&gt;/.butler/training/graph_export.json. Then use Harvester below (fill the new "Graph export" field with .butler/training/graph_export.json) + tight scope/ignore + small batch/steps. This is how you label the rich set without exploding LLM cost.</div>
    </div>

    <div class="card">
        <h3 style="margin-top:0">Harvester</h3>
        <form id="harv-form" onsubmit="submitHarvester(event)">
            <div class="row">
                <div>
                    <label>Repo path</label>
                    <input id="harv-repo" type="text" value="/projects/test_repos/bat">
                </div>
                <div>
                    <label>LLM base</label>
                    <input id="harv-llm" type="text" value="http://localhost:4000">
                    <div class="hint" style="font-size:0.7rem">Use <b>http://litellm:4000</b> for docker llm-stack.</div>
                </div>
                <div>
                    <label>API Key / Bearer token (for litellm master key)</label>
                    <input id="harv-apikey" type="password" value="">
                    <div class="hint" style="font-size:0.7rem">Enter sk-dummy-key-not-real (or your key). Sent as Authorization: Bearer ...</div>
                </div>
            </div>
            <div class="row">
                <div>
                    <label>Model (litellm alias)</label>
                    <input id="harv-model" type="text" value="grok-4.3">
                </div>
                <div>
                    <label>Query</label>
                    <input id="harv-query" type="text" value="core public API, traits, important impls, FFI boundaries">
                </div>
            </div>
            <div class="row">
                <div>
                    <label>Batch size</label>
                    <input id="harv-batch" type="number" value="5">
                </div>
                <div>
                    <label>Max steps</label>
                    <input id="harv-steps" type="number" value="5">
                </div>
            </div>
            <div>
                <label>Scope paths (comma-separated) — use distinctive terms so sub-crates don't leak in (e.g. bevy="crates", pyo3="pyclass,instance,marker,err,any,dict", not just "src")</label>
                <input id="harv-scope" type="text" value="">
            </div>
            <div>
                <label>Avoid / ignore paths (comma-separated, e.g. pyo3-build-config,tests,examples,noxfile) — excludes matching files even if they match scope</label>
                <input id="harv-ignore" type="text" value="tests,docs,examples,benchmarks,tools,questions,imgs,assets,.github,.git,.faq,build,dist,.venv,__pycache__">
            </div>
            <div>
                <label>Graph export (butler_export) for ID alignment — e.g. .butler/training/graph_export.json (use after Build Training Bundle; optional but recommended for rich training sets)</label>
                <input id="harv-export-path" type="text" value="">
            </div>
            <div>
                <label>Accuracy rules (important for training label quality)</label>
                <div style="font-size:0.8rem; margin-top:4px;">
                    <div><input type="checkbox" id="harv-acc-note" checked> <label for="harv-acc-note">require exploration_note</label></div>
                    <div><input type="checkbox" id="harv-acc-rej" checked> <label for="harv-acc-rej">require explicit rejections</label></div>
                    <div><input type="checkbox" id="harv-acc-edge" checked> <label for="harv-acc-edge">require reason on every edge</label></div>
                    <div>min hard negs/batch: <input id="harv-acc-minneg" type="number" value="1" style="width:3.5em"></div>
                </div>
            </div>
            <div>
                <label>Frontier (leverage rich nodes + containment)</label>
                <div style="font-size:0.8rem; margin-top:4px;">
                    <div><input type="checkbox" id="harv-fr-ast" checked> <label for="harv-fr-ast">AST / nesting distance</label></div>
                    <div><input type="checkbox" id="harv-fr-deg" checked> <label for="harv-fr-deg">degree (hubs)</label></div>
                    <div><input type="checkbox" id="harv-fr-bm" checked> <label for="harv-fr-bm">BM25</label></div>
                </div>
            </div>
            <div style="font-size:0.8rem; margin-top:4px;">
                <div><input type="checkbox" id="harv-poly" checked> <label for="harv-poly">Include interconnect edges (polyglot)</label></div>
                <div><input type="checkbox" id="harv-loadprev" checked> <label for="harv-loadprev">Load previous context (resume)</label></div>
            </div>
            <div>
                <label>Temperature</label> <input id="harv-temp" type="number" step="0.1" value="0.1" style="width:4em">
            </div>
            <div>
                <label><input type="checkbox" id="harv-export" checked> Export fat to &lt;repo&gt;/.butler/fat.json (inside the mounted repo, optional)</label>
            </div>
            <div style="margin-top:8px">
                <button type="submit">Run Harvester</button>
                <button type="button" onclick="stopHarvester()">Stop</button>
            </div>
        </form>
        <div id="harv-status" style="margin-top:8px; white-space:pre-wrap; font-family:monospace; font-size:0.8rem; background:#0d1117; border:1px solid #30363d; padding:8px; min-height:60px; max-height:200px; overflow:auto;"></div>
        <div class="hint">Runs the harvester (can take time). Polls status. Use Stop to request cancel. Result in /tmp/.... Full config (accuracy, frontier, etc) now exposed for rich training graphs.</div>
    <div class="hint" style="color:#f85149">For docker: litellm:4000. <b>For rich training graphs:</b> use Graph export field + tight scope/ignore. Tune Accuracy (more rejections = better training signal) and Frontier toggles (AST/degree now powerful with rich nodes). Small batch + resume via "Load previous".</div>
    </div>

    <div class="card">
        <h3 style="margin:0 0 8px">Response</h3>
        <pre id="output">Submit a request to see raw JSON here...</pre>
    </div>

    <div class="card">
        <small>Tip: For agents use the MCP endpoint or /mcp/manifest. This dashboard is for quick human inspection only.</small>
    </div>
</div>

<script>
async function submitOrchestrate(e) {
    e.preventDefault();
    const project = document.getElementById('project').value.trim();
    const goal = document.getElementById('goal').value;
    const scopeRaw = document.getElementById('scope').value.trim();
    const ignoreRaw = document.getElementById('ignore').value.trim();
    const target = document.getElementById('target').value.trim();
    const detail = document.getElementById('detail') ? document.getElementById('detail').value : 'short';

    let scope_paths = [];
    if (scopeRaw) {
        if (scopeRaw.startsWith('[')) {
            try { scope_paths = JSON.parse(scopeRaw); } catch (_) { scope_paths = scopeRaw.split(',').map(s => s.trim()).filter(Boolean); }
        } else {
            scope_paths = scopeRaw.split(',').map(s => s.trim()).filter(Boolean);
        }
    }

    let ignore_paths = [];
    if (ignoreRaw) {
        ignore_paths = ignoreRaw.split(',').map(s => s.trim()).filter(Boolean);
    }

    const payload = {
        mcp_tool_name: "butler_orchestrate",
        project,
        goal,
        scope_paths,
        ignore_paths,
        detail
    };
    if (target) payload.target_symbol = target;

    const out = document.getElementById('output');
    out.textContent = 'Loading...';

    try {
        const res = await fetch('/context', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        if (!res.ok) {
            throw new Error(`HTTP ${res.status}: ${await res.text()}`);
        }
        const text = await res.text();
        try {
            const j = JSON.parse(text);
            if (j && j.error) {
                out.textContent = `Error: ${j.error}${j.details ? ' — ' + j.details : ''}`;
            } else {
                out.textContent = text;
            }
        } catch (_) {
            out.textContent = text;
        }
    } catch (err) {
        out.textContent = 'Error: ' + (err.message || err);
    }
}

async function exportButlerGraph() {
    const project = document.getElementById('project').value.trim();
    const scopeRaw = document.getElementById('scope').value.trim();
    const ignoreRaw = document.getElementById('ignore').value.trim();
    const out = document.getElementById('output');
    out.textContent = 'Exporting Butler graph (skinny export, respecting scope+ignore)...';

    let scope_paths = [];
    if (scopeRaw) {
        if (scopeRaw.startsWith('[')) {
            try { scope_paths = JSON.parse(scopeRaw); } catch (_) { scope_paths = scopeRaw.split(',').map(s => s.trim()).filter(Boolean); }
        } else {
            scope_paths = scopeRaw.split(',').map(s => s.trim()).filter(Boolean);
        }
    }
    let ignore_paths = [];
    if (ignoreRaw) {
        ignore_paths = ignoreRaw.split(',').map(s => s.trim()).filter(Boolean);
    }

    try {
        const res = await fetch('/export-graph', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ project, scope_paths, ignore_paths })
        });
        const text = await res.text();
        out.textContent = text;
    } catch (err) {
        out.textContent = 'Error: ' + (err.message || err);
    }
}

async function submitTrainingBundle(e) {
    e.preventDefault();
    const repo = document.getElementById('bundle-repo').value.trim();
    const outDir = document.getElementById('bundle-out').value.trim();
    const status = document.getElementById('bundle-status');
    const out = document.getElementById('output');
    status.textContent = 'Building structural training bundle (heavy parse + rich nodes/edges)...';
    out.textContent = 'See status below.';

    try {
        const res = await fetch('/build-training-bundle', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ repo, out_dir: outDir })
        });
        const text = await res.text();
        status.textContent = text;
        if (res.ok) {
            out.textContent = 'Structural bundle ready in the repo\'s .butler/training/. Now use the Harvester section manually (configure your LLM etc.) pointing at that graph_export.json.';
        }
    } catch (err) {
        status.textContent = 'Error: ' + (err.message || err);
    }
}

async function submitHarvester(e) {
    e.preventDefault();
    const repo = document.getElementById('harv-repo').value.trim();
    const llm = document.getElementById('harv-llm').value.trim();
    const model = document.getElementById('harv-model').value.trim();
    const query = document.getElementById('harv-query').value.trim();
    const batch = parseInt(document.getElementById('harv-batch').value) || 5;
    const steps = parseInt(document.getElementById('harv-steps').value) || 5;
    const scopeRaw = document.getElementById('harv-scope').value.trim();
    const ignoreRaw = document.getElementById('harv-ignore').value.trim();
    const apiKey = document.getElementById('harv-apikey').value.trim();
    const exportToRepo = document.getElementById('harv-export').checked;
    const butlerExport = document.getElementById('harv-export-path').value.trim();

    let scope = [];
    if (scopeRaw) {
        scope = scopeRaw.split(',').map(s => s.trim()).filter(Boolean);
    }
    let ignore = [];
    if (ignoreRaw) {
        ignore = ignoreRaw.split(',').map(s => s.trim()).filter(Boolean);
    }

    const requireNote = document.getElementById('harv-acc-note').checked;
    const requireRej = document.getElementById('harv-acc-rej').checked;
    const requireEdge = document.getElementById('harv-acc-edge').checked;
    const minNeg = parseInt(document.getElementById('harv-acc-minneg').value) || 1;
    const useAst = document.getElementById('harv-fr-ast').checked;
    const useDeg = document.getElementById('harv-fr-deg').checked;
    const useBm = document.getElementById('harv-fr-bm').checked;
    const poly = document.getElementById('harv-poly').checked;
    const loadPrev = document.getElementById('harv-loadprev').checked;
    const temp = parseFloat(document.getElementById('harv-temp').value) || 0.1;

    const payload = {
        repo, llm_base: llm, model, query, batch_size: batch, max_steps: steps,
        scope, ignore, api_key: apiKey || undefined,
        export_to_repo: exportToRepo, butler_export: butlerExport || undefined,
        require_exploration_note: requireNote,
        require_explicit_rejections: requireRej,
        require_reason_on_every_edge: requireEdge,
        min_hard_negatives_per_batch: minNeg,
        use_ast_distance: useAst,
        use_degree: useDeg,
        use_bm25: useBm,
        include_interconnect: poly,
        load_previous_context: loadPrev,
        temperature: temp
    };

    const out = document.getElementById('output');
    const status = document.getElementById('harv-status');
    const btn = e.target.querySelector('button[type="submit"]');
    if (btn) { btn.disabled = true; btn.textContent = 'Running...'; }
    out.textContent = 'Harvest started...';
    status.textContent = 'Waiting for logs (loading CodeGraph + LLM calls can take time on big repos)...';

    try {
        const res = await fetch('/harvester', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        if (!res.ok) {
            throw new Error(`HTTP ${res.status}: ${await res.text()}`);
        }
        const text = await res.text();
        try {
            const j = JSON.parse(text);
            if (j && j.error) {
                out.textContent = `Error: ${j.error}${j.details ? ' — ' + j.details : ''}${j.repo ? ' (' + j.repo + ')' : ''}`;
            } else {
                out.textContent = text;
            }
        } catch (_) {
            out.textContent = text;
        }
        // Start polling *after* the kickoff has returned. This guarantees the handler
        // has already written the START banner + any ERROR to live.log, avoiding races
        // where the first status poll sees stale data from a previous run.
        startStatusPoll(status);
    } catch (err) {
        out.textContent = 'Error: ' + (err.message || err);
        if (btn) { btn.disabled = false; btn.textContent = 'Run Harvester'; }
    }
}

let statusTimeout = null;
let accumulatedLogs = ""; // persistent across polls to keep full history

function startStatusPoll(statusEl) {
    if (statusTimeout) clearTimeout(statusTimeout);
    accumulatedLogs = "";
    // Force a clean visual reset so previous run's COMPLETE or old transcript disappears immediately.
    statusEl.textContent = 'Waiting for first logs...';

    async function poll() {
        try {
            const res = await fetch('/harvester/status');
            if (!res.ok) {
                throw new Error(`HTTP ${res.status}`);
            }
            const data = await res.json();

            // Append new logs (server sends recent/tail; dedup overlapping windows)
            if (data.recent_logs && data.recent_logs.length > 0) {
                const newChunk = data.recent_logs.join('\n');
                if (!accumulatedLogs.endsWith(newChunk)) {
                    accumulatedLogs += (accumulatedLogs ? '\n' : '') + newChunk;
                }
            }

            let outText = accumulatedLogs;
            if (data.fat) {
                outText += '\n\n-- LATEST FAT --\n' + JSON.stringify(data.fat, null, 2);
            }
            statusEl.textContent = outText || 'Waiting for first logs (graph load or first LLM roundtrip may take 5-30s)...';
            statusEl.scrollTop = statusEl.scrollHeight;

            if (data.running) {
                statusTimeout = setTimeout(poll, 2000);
            } else {
                statusEl.textContent += '\n\n[HARVEST COMPLETE]';
                const btn = document.querySelector('#harv-form button[type="submit"]');
                if (btn) { btn.disabled = false; btn.textContent = 'Run Harvester'; }
            }
        } catch (e) {
            statusEl.textContent += '\n[poll error: ' + e + ']... retrying';
            statusTimeout = setTimeout(poll, 2000);
        }
    }

    poll();
}

async function stopHarvester() {
    try {
        const res = await fetch('/harvester/cancel', { method: 'POST' });
        const txt = await res.text();
        const status = document.getElementById('harv-status');
        status.textContent += '\nStop requested: ' + txt;
        // Re-enable immediately on explicit stop; the running flag will clear shortly
        const btn = document.querySelector('#harv-form button[type="submit"]');
        if (btn) { btn.disabled = false; btn.textContent = 'Run Harvester'; }
    } catch (err) {
        const status = document.getElementById('harv-status');
        status.textContent = 'Stop error: ' + err;
        const btn = document.querySelector('#harv-form button[type="submit"]');
        if (btn) { btn.disabled = false; btn.textContent = 'Run Harvester'; }
    }
}

// Set build time
document.getElementById('build-time').textContent = new Date().toLocaleString();
</script>
</body>
</html>
"#;
