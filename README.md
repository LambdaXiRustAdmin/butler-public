# Butler

**Structural maps for coding agents** — callers, callees, reverse spine, dual-stack bridges.

**Not a RAG.** You get a **map pack**, not a whole-tree dump. Agents should Trace a symbol instead of inventing structure with `rg`.

| | |
|--|--|
| **Product** | **Butler** (tools `butler_*`, env `BUTLER_*`, cache `.butler/`) |
| **License** | MIT |
| **Stage** | **Map Alpha** — careful dogfood, not GA |

---

## Alpha (read this)

Keepers and fixed gates are green on the tips we ship; **arbitrary repos may soft-fail**.

| Expect | Do not expect |
|--------|----------------|
| Deterministic Trace dossier when the warehouse is warm | Perfect call graphs for every language / framework magic |
| Honest empty / disambiguate / BUILDING (not silent lies) | “Safe to delete” when callers = 0 (callbacks, DI, IPC…) |
| Export / IPC bridges on dual-stack keepers | Install-and-forget SaaS |
| Cold first hit may say `BUILDING` — **retry the same request** | Instant Complete on a huge monorepo with no warm |

**Prove the tip (optional, after server is up):**

```bash
python3 scripts/butler_alpha_gate.py --base http://127.0.0.1:8002
```

Setup: [`docs/ALPHA_SETUP.md`](./docs/ALPHA_SETUP.md) · Ops: [`docs/OPS.md`](./docs/OPS.md) · Readiness notes: [`docs/alpha-readiness.md`](./docs/alpha-readiness.md)

---

## 20-minute stranger path

### 0. Prerequisites

- Rust toolchain (edition 2021; recent stable) **or** a portable zip from CI (`butler-windows-x64.zip` / `butler-linux-x64.zip` via `.github/workflows/release.yml`)
- A **local absolute path** to some code (`/home/you/code/my-app`)

**Windows portable:** unzip → `butler.exe ui` → browser `/setup` (see `packaging/README-WINDOWS.txt`). Keep the three `.exe` files in one folder.

### 1. Build & start the server

```bash
git clone <this-repo> && cd butler   # directory name may vary
cargo build --release -p cli

# One command after release build or install.sh:
./target/release/butler ui
# → starts butler-server if needed, opens http://127.0.0.1:8002/setup?spawned=1
```

Or: `./install.sh` — installs bins and **runs `butler ui`** (starts server + opens `/setup`).

Uninstall host install: [packaging/UNINSTALL.md](./packaging/UNINSTALL.md).

Manual server only:

```bash
./target/release/butler-server
# then open http://127.0.0.1:8002/setup
```

- **Setup / proof of life (start here):** http://127.0.0.1:8002/setup  
- **Operator tools** (export / harvest / full form): http://127.0.0.1:8002/ops (also `/`)  
- Health: `curl -sS http://127.0.0.1:8002/mcp/health`

### 2. Point Butler at your code + one Trace

**Replace** `/ABS/PATH/TO/REPO` with a real absolute root (not `~/…` unexpanded).

```bash
# Optional: register root into the running server (async)
curl -sS -X POST http://127.0.0.1:8002/warm \
  -H 'Content-Type: application/json' \
  -d '{"roots":["/ABS/PATH/TO/REPO"]}'

# One Trace (compact dossier in `content`, machine pack in `structured`)
curl -sS -X POST http://127.0.0.1:8002/context \
  -H 'Content-Type: application/json' \
  -d '{
    "project": "/ABS/PATH/TO/REPO",
    "goal": "TraceBlastRadius",
    "target_symbol": "main",
    "detail": "compact"
  }'
```

**Helper (health → warm → Trace with BUILDING retries):**

```bash
./scripts/smoke_stranger.sh /ABS/PATH/TO/REPO main
```

| If you see… | Do this |
|-------------|---------|
| `status: BUILDING` / hydrating | Retry the **same** Trace (or re-run smoke). Do **not** wait on `edge_builds` in health for “ready to Trace”. |
| Disambiguate / many locations | Pin `"scope_paths": ["src/path/or/file.rs"]` and Trace again |
| Empty callers | Read the honesty line — may be callback / export / external; not automatic “dead code” |

Offline cache warm (nice before first Trace on larger trees):

```bash
./target/release/butler warm -r /ABS/PATH/TO/REPO
./target/release/butler warm -r /ABS/PATH/TO/REPO --full --server http://127.0.0.1:8002
```

### 3. Wire an agent (optional, same day)

**MCP stdio** (server must already be running):

```json
{
  "mcpServers": {
    "butler": {
      "command": "/ABS/PATH/TO/butler/target/release/mcp",
      "args": ["--stdio"],
      "env": {
        "BUTLER_URL": "http://127.0.0.1:8002"
      }
    }
  }
}
```

Build the bridge: `cargo build --release -p cli` (includes `mcp`).

**Skill (portable contract):** [`skills/butler-ask/SKILL.md`](./skills/butler-ask/SKILL.md) — prefer `butler_ask` for structure; follow `next:`; no structural grep when Butler is available.

```json
{
  "project": "/ABS/PATH/TO/REPO",
  "symbol": "createClient",
  "scope_paths": ["src/"]
}
```

---

## Three install questions

| # | Question | Typical answer |
|---|----------|----------------|
| **1** | Where does Butler live? | This clone + `target/release/*` or `./install.sh` → `~/.local` |
| **2** | Where are program files? | Absolute roots passed as `project` every Trace |
| **3** | Where is the server? | **Local native** (default) · remote URL · Docker (optional) |

Docker is optional. Full one-pager: [`docs/ALPHA_SETUP.md`](./docs/ALPHA_SETUP.md).

Default bind is **loopback** (`127.0.0.1`). Optional password: see [`docs/OPS.md`](./docs/OPS.md) before binding `0.0.0.0`.

---

## What you get (product)

- **Persistent CodeGraph** — skeleton scan, background FullEdge, multi-repo warehouse  
- **Trace / Find / Arch** — via `POST /context` or MCP `butler_ask`  
- **Receipts** — confidence / basis / edges; disambiguate for homonyms; reverse CALL spine when present  
- **Dual-stack floor** — Export / IPC bridges (not CALL soup)  
- **Cold usable partial** — BUILDING + TOC; edges continue in background  

**Optional / advanced (not required for map Alpha):** neural ranking (`use_neural`), Harvester gold labels, Docker compose packaging.

---

## MCP tools (short)

| Tool | Role |
|------|------|
| **`butler_ask`** | **Primary.** `project` required; symbol / scope / mode |
| `butler_orchestrate` | Explicit goals (power tool) |
| `butler_help` | Usage contract |

| Transport | Human | Machine |
|-----------|--------|---------|
| HTTP `POST /context` | `content` | `structured` |
| MCP | `content[0].text` | `structuredContent` |

---

## Configuration

Layered (low → high): defaults → `~/.config/butler/config.toml` → `.butler/config.toml` → `BUTLER__*` env.

**Full knob catalog:** [`cli/config.example.toml`](cli/config.example.toml)

```toml
[server]
port = 8002
warm_roots = ["/home/you/projects/my-app"]
max_cached_graphs = 32

[analysis]
edge_build_thread_pct = 0.75
trace_max_fan_out = 20

[agent]
# Map Alpha: leave neural off unless you intentionally opt in
use_neural = false
```

### Environment-only (ops)

| Variable | Purpose |
|----------|---------|
| `BUTLER_WARM_ROOTS` | Boot-warm roots (`:` or `,`) |
| `BUTLER_QUERY_PARALLEL` | Max concurrent `/context` |
| `BUTLER_FULLEDGE_PARALLEL` | Max concurrent FullEdge jobs (default 2) |
| `BUTLER_EDGE_THREADS` | Absolute edge-build threads |
| `BUTLER_VERBOSE=1` | Per-request logs |
| `BUTLER_FORCE_RESCAN=1` | Ignore disk cache |
| `BUTLER_GNN_WEIGHTS` | Force weights path |
| `BUTLER_HOST_MOUNT` / `BUTLER_CONTAINER_MOUNT` | Docker path rewrite |
| `BUTLER_PASSWORD` / `BUTLER_API_TOKEN` | Optional auth secret |

---

## CLI

```bash
butler warm -r /path/to/repo
butler warm -r /a -r /b --full --server http://127.0.0.1:8002
butler context "rate_limit" -r /path/to/repo --depth 2
butler export-graph -r /path/to/repo
# Expert: gold labels (needs a harvest template you supply)
# butler harvest -r /path/to/repo -t /path/to/template.json -o /tmp/fat.json
```

---

## Support

Optional tips: [ko-fi.com/lambdawisperer](https://ko-fi.com/lambdawisperer) — appreciated, never required.

---

## License

MIT — see [LICENSE](./LICENSE).
