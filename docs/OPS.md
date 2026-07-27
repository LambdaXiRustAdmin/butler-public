# Butler ops runbook (A5)

Short guide so **you are not required in the room**. Pair with [ALPHA_SETUP.md](./ALPHA_SETUP.md).

---

## 0 — Safe defaults (remember these)

| Mode | `server.host` | Password |
|------|---------------|----------|
| Laptop / local agents | **`127.0.0.1`** (default since A5) | optional |
| LAN / remote / Docker port publish | `0.0.0.0` | **set** `server.password` / `BUTLER_PASSWORD` |

Username defaults to **hostname** (`BUTLER_USERNAME` to override).

When password is set: clients send `Authorization: Bearer <secret>` (MCP: same env as server).

---

## 1 — Start server (native — no Docker)

```bash
cd /path/to/butler   # this clone
cargo build --release -p cli
./target/release/butler-server
# process name: butler-server · default http://127.0.0.1:8002
```

Optional config (`~/.config/butler/config.toml` or `.butler/config.toml`):

```toml
[server]
host = "127.0.0.1"
port = 8002
# username = "my-laptop"
# password = "long-secret"
warm_roots = ["/home/you/projects/my-app"]
```

**Health**

```bash
curl -sS http://127.0.0.1:8002/mcp/health | jq .
# If password set and no token: {"status":"ok","auth_required":true} only
curl -sS -H "Authorization: Bearer $BUTLER_PASSWORD" http://127.0.0.1:8002/mcp/health | jq .
```

---

## 2 — Docker (optional)

Only if you run Butler via Docker Compose. Image does **not** hot-reload the binary after code changes.

```bash
cd /path/to/your-compose
# After server/auth/context changes:
docker compose build butler && docker compose up -d butler
docker logs -f your-butler-container
```

Set bind + secret in compose env if the port is published beyond localhost:

```yaml
# illustrative
environment:
  BUTLER__SERVER__HOST: "0.0.0.0"
  BUTLER_PASSWORD: "…"
```

Path rewrite when code is mounted: `BUTLER_HOST_MOUNT` + `BUTLER_CONTAINER_MOUNT` (see ALPHA_SETUP).

---

## 3 — MCP client (agents)

```json
{
  "mcpServers": {
    "butler": {
      "command": "/abs/path/to/mcp",
      "args": ["--stdio"],
      "env": {
        "BUTLER_URL": "http://127.0.0.1:8002",
        "BUTLER_PASSWORD": ""
      }
    }
  }
}
```

- `BUTLER_URL` = where the server is (question 3).  
- `BUTLER_PASSWORD` = same as server when locked; omit/empty when open.  
- Skill: `skills/butler-ask/SKILL.md`.

---

## 4 — Agent-facing contract (ops should know)

| Signal | Meaning | What to do |
|--------|---------|------------|
| `status: BUILDING` / `=== Building` | Cold warehouse; usable partial | Retry same `/context`; do **not** poll `edge_builds` for hydrate |
| `edge_builds` | FullEdge progress only | Not “is it loaded?” — use `loaded` / ready |
| `domain=disambiguate` | Multi-loc homonym | Pin **one** `scope_paths` path, re-Trace |
| `0 CALL callers` | Honest empty reverse | Check `peer_callers` / bridges; not “safe to delete” |
| `auth_required` on health | Password on, no token | Send Bearer or use open loopback |

Warm without waiting on the agent:

```bash
./target/release/butler warm -r /path/to/repo --server http://127.0.0.1:8002
# or boot: BUTLER_WARM_ROOTS=/a:/b
```

---

## 5 — Gates & dogfood (tip check)

```bash
# Engine only (no LLM)
python3 scripts/butler_alpha_gate.py --lanes health,integrity,watcher,spectacular

# Full accuracy + Qwen agent (needs LiteLLM on :4000)
# If LITELLM_MASTER_KEY is unset, dogfood tries docker container `litellm`.
# Optional extended probes (when present): hole / spectacular / watcher scripts
```

Manual key from compose container (if needed):

```bash
export LITELLM_MASTER_KEY="$(docker inspect litellm --format '{{range .Config.Env}}{{println .}}{{end}}' | sed -n 's/^LITELLM_MASTER_KEY=//p' | tr -d '\r\n')"
```

Receipts: write under `/tmp` or a local `receipts/` folder (not required in-tree).

---

## 6 — Optional nightly (cron)

```bash
# example: weekdays 06:15 — accuracy only is safer for unattended
15 6 * * 1-5 cd /path/to/butler && \
  python3 scripts/butler_alpha_gate.py --lanes health,integrity,watcher,spectacular \
  >> /tmp/alpha-gate-cron.log 2>&1
```

Full dogfood with agent needs a stable LLM key in the environment (or docker `litellm` running).

---

## 7 — Troubleshooting

| Symptom | Check |
|---------|--------|
| Connection refused | Is `butler-server` up? Port? |
| 401 unauthorized | Password set? Client `BUTLER_PASSWORD` match? |
| Empty `loaded` in health | Open health with password only returns liveness — send token |
| Docker still old Trace | Rebuild image (binary is copied in; host mount is not the binary) |
| Dogfood agent FATAL models 401 | `LITELLM_MASTER_KEY` missing — load from env or docker |
| Cold Trace “hang” | Look for BUILDING partial + `next:`; rewalk, don’t invent edge_builds wait |

---

## Related

| Doc | Role |
|-----|------|
| [ALPHA_SETUP.md](./ALPHA_SETUP.md) | Install three questions |
| [alpha-readiness.md](./alpha-readiness.md) | Alpha bar |
| [cli/config.example.toml](../cli/config.example.toml) | Full knobs |
| [Dockerfile](../Dockerfile) | Image build context |
