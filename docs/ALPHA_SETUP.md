# Butler Alpha — setup one-pager

**Not a RAG.** Butler hands agents a **code map** (callers, callees, reverse spine, dual-stack bridges), not a file dump.

**Names:** product / process / tools = **Butler**. Optional external brand experiments are parked for later.

---

## Three install questions (same as the old config page)

On first install you answer three things. Everything else is optional polish.

| # | Question | What it means | Where it lands |
|---|----------|---------------|----------------|
| **1** | **Where does Butler live?** | Binaries + optional GNN weights on *this* machine | `install.sh` → `~/.local/bin/butler`, `~/.local/share/butler/weights/` · or cargo build in-repo |
| **2** | **Where are your program files?** | Absolute roots of the code you want mapped | `project` on every Trace · `server.warm_roots` / `BUTLER_WARM_ROOTS` · dashboard project field |
| **3** | **Where is the server?** | Who serves HTTP/MCP — **not** “must be Docker” | See modes below |
| **4** | **Who am I / is it locked?** | Display username (default **computer hostname**) + optional **password** | `server.username` / `server.password` · env below |

Docker is **one** answer to question 3, not the product.

### Identity & optional password (security)

The server can expose **code structure for every warm root** (Trace, harvest, export). Treat a LAN bind without a secret as public.

| Knob | Default | Env |
|------|---------|-----|
| `server.username` | OS hostname | `BUTLER_USERNAME` |
| `server.password` | *empty = open* | `BUTLER_PASSWORD` or `BUTLER_API_TOKEN` |
| `server.host` | **`127.0.0.1`** (A5) | `BUTLER__SERVER__HOST` — use `0.0.0.0` for LAN/Docker |

**When password is set**, clients must send:

```http
Authorization: Bearer <password>
```

or Basic `username:password`, or header `X-Butler-Token: <password>`.

- MCP stdio: set the same env on the **mcp** process (`BUTLER_PASSWORD` / `BUTLER_API_TOKEN`).
- Unauthenticated `GET /mcp/health` only returns liveness (`status=ok`, `auth_required=true`) — **not** loaded roots.
- Local-only without password: set `host = "127.0.0.1"` so the map is not on the LAN.

```toml
[server]
host = "127.0.0.1"
username = "my-laptop"          # optional; default = hostname
# password = "long-random-secret"  # optional but recommended for remote/LAN
```

---

## 1 — Where does Butler live?

### Preferred: local install

```bash
cd /path/to/butler   # this clone
./install.sh
# → ~/.local/bin/butler
# → ~/.local/share/butler/weights/gnn_trained_global.bin (if present)
```

Or in-tree release binaries (dev / dogfood):

```bash
cargo build --release -p cli
# ./target/release/butler-server   # HTTP + dashboard
# ./target/release/mcp             # MCP stdio bridge
# ./target/release/butler          # CLI warm/export/harvest
```

### Config files (layered, low → high)

1. Built-in defaults  
2. **Global:** `~/.config/butler/config.toml`  
3. **Workspace:** `.butler/config.toml` (cwd when server starts)  
4. **Env:** `BUTLER__SERVER__PORT=8002`, `BUTLER_WARM_ROOTS=…`

Full knob list: [`cli/config.example.toml`](../cli/config.example.toml).

---

## 2 — Where are your program files?

Butler maps **absolute project roots** on disk. Agents pass that path as `project`.

| Situation | What to pass |
|-----------|----------------|
| Native server, code on host | `/home/you/projects/my-app` |
| Server in Docker, code bind-mounted | Container path, e.g. `/projects/my-app` (with `BUTLER_HOST_MOUNT` / `BUTLER_CONTAINER_MOUNT` rewrite) |
| Multi-repo warehouse | Each root is a separate `project`; warm several |

**Preload (optional):** cold Trace still returns a usable `BUILDING` partial. Warm if you want hot first hits:

```bash
butler warm -r /home/you/projects/my-app
butler warm -r /a -r /b --full --server http://127.0.0.1:8002
# or boot: export BUTLER_WARM_ROOTS=/a:/b
# or config: server.warm_roots = ["/a", "/b"]
```

Caches live under each project’s **`.butler/`** (graph shards, memo). Do not treat `.butler` as source.

---

## 3 — Where is the server?

Pick **one** mode. Agents only need a URL (or stdio MCP that points at it).

### A) Local native (default for developers)

```bash
./target/release/butler-server
# or after install.sh: ensure butler-server is on PATH / run from target/release
```

- Listens: **`http://127.0.0.1:8002`** by default (A5 — not all interfaces)  
- Dashboard: open that URL in a browser  
- Health: `curl -sS http://127.0.0.1:8002/mcp/health`  
- **No Docker required.**  
- Ops detail: [OPS.md](./OPS.md)

### B) Remote native

Same binary on a machine that can see the code (or NFS/mount of program files):

```bash
# on server host
BUTLER__SERVER__HOST=0.0.0.0 ./target/release/butler-server
```

Clients set:

```bash
export BUTLER_URL=http://that-host:8002
```

MCP stdio bridge still runs **on the agent machine** and talks HTTP to `BUTLER_URL`.

### C) Docker (optional stack mode)

Useful when you already run your compose stack / compose. Example sibling stack:

- Compose service `butler` → host **:8002**  
- Mount program tree → e.g. `/projects`  
- Set `BUTLER_HOST_MOUNT` + `BUTLER_CONTAINER_MOUNT` so host paths and container paths stay honest  

Canonical rebuild (ops, not required for Alpha product):

```bash
cd /path/to/your-compose-stack && docker compose build butler && docker compose up -d butler
```

**Agents still use the same contract:** HTTP `:8002` + absolute `project` paths **as the server sees them**.

---

## Agent connection (after the three answers)

### MCP stdio (Cursor / Claude Desktop / Roo / …)

```json
{
  "mcpServers": {
    "butler": {
      "command": "/absolute/path/to/mcp",
      "args": ["--stdio"],
      "env": {
        "BUTLER_URL": "http://127.0.0.1:8002"
      }
    }
  }
}
```

- `command` = your **local** `mcp` binary (question 1).  
- `BUTLER_URL` = server mode (question 3).  
- Tool **`butler_ask`**: `project` = program root (question 2).

Skill contract: [`skills/butler-ask/SKILL.md`](../skills/butler-ask/SKILL.md).

### HTTP only

```bash
curl -sS -X POST http://127.0.0.1:8002/context \
  -H 'Content-Type: application/json' \
  -d '{"project":"/home/you/projects/my-app","goal":"trace","target_symbol":"main","detail":"compact"}'
```

**Stranger smoke (health → warm → Trace with BUILDING retries):** from repo root, after `butler-server` is up:

```bash
./scripts/smoke_stranger.sh /home/you/projects/my-app main
```

See also the **20-minute stranger path** in [`README.md`](../README.md).

| Field | Use |
|-------|-----|
| `project` | Absolute root (question 2) |
| `goal` / `symbol` / `mode` | Trace / Find / Arch |
| `scope_paths` | Pin package or file when multi-loc |
| Response `content` | Human dossier |
| Response `structured` | Machine pack |

---

## What Trace proves (and does not)

| Proves | Does **not** prove |
|--------|---------------------|
| CALL callers/callees | All “usage” (callbacks, DI, routers) |
| Dual-stack Export / IPC bridges | Safe to delete when callers empty |
| Reverse CALL spine (when non-empty) | Framework magic wiring |
| Multi-loc → disambiguate / pin | RAG / full-file dump |

**0 CALL callers ≠ dead code** — see `peer_callers`, bridges, and receipt confidence.

---

## Smoke checklist (Alpha)

```bash
# 1) Server up
curl -sS http://127.0.0.1:8002/mcp/health

# 2) Map honesty + gates (no LLM)
python3 scripts/butler_alpha_gate.py --lanes health,integrity,watcher,spectacular

# 3) Optional full dogfood (needs LiteLLM + key for agent phase)
export LITELLM_MASTER_KEY=…   # from your LLM gateway / compose
# Optional: smoke_stranger.sh /ABS/ROOT Symbol
```

Gate writes a JSON report path in its stdout (or under a local receipts folder if you configure one).

---

## Product loop (one sentence)

**Warm a root → Trace a seed (pin if asked) → edit from callers / spine / bridges → watcher keeps the map honest.**

Dashboard at `/` is for **human** inspect/harvest; agents should use **MCP / `/context`**, not scrape the HTML.

---

## Related

| Doc | Role |
|-----|------|
| [OPS.md](./OPS.md) | **A5** day-2 ops (auth, warm, dogfood, Docker rebuild) |
| [README.md](../README.md) | Full knobs + CLI |
| [cli/config.example.toml](../cli/config.example.toml) | Config catalog |
| [alpha-readiness.md](./alpha-readiness.md) | Alpha bar / A1–A6 |
| [skills/butler-ask/SKILL.md](../skills/butler-ask/SKILL.md) | Agent structural contract |
