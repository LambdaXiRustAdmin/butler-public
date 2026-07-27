---
name: butler-ask
description: >
  HOW-TO for Butler structural CodeGraph. After reading this skill you MUST call the MCP
  tool butler_ask (not this skill again). Use for who-calls, blast radius, architecture map,
  API edit map, dual-stack/FFI bridges, reverse call path, homonyms. Prefer over grep/rg/list_dir.
  Triggers: architecture, callers, callees, blast radius, call path, reverse spine, butler,
  CodeGraph, Trace, Arch, scope_paths, butler_ask.
---

# Butler — agent skill (portable)

## CRITICAL — skill ≠ tool (read this first)

| This skill (`butler-ask`) | MCP tool `butler_ask` |
|---------------------------|------------------------|
| Documentation only | Real query to Butler on `:8002` |
| Loading it returns **these instructions** | Calling it returns a **map/Trace dossier** |
| Does **not** hit the server | Stdio `mcp` → `BUTLER_URL` → `butler-server` |

**After you load this skill, your next structural step is an MCP tool call — not another skill load.**

1. Find the MCP tool named **`butler_ask`** (Roo may show `mcp--butler--butler_ask` or `mcp--butler_context--butler_ask` — same tool).
2. Call it with JSON args (see below). Do **not** pass query args only to the skill mechanism.
3. If **no** MCP tool `butler_ask` / `butler_orchestrate` appears in your tool list → say **“Butler MCP not connected”** (fix client config / enable server / restart). Do **not** claim the graph server is offline solely because a **skill** returned text.
4. Skill text is never a Trace/Arch result. If you only received markdown from a skill, **you have not queried Butler yet.**

**Minimal first MCP call (example):**

```json
{
  "project": "/projects/lambda-wisperer",
  "mode": "arch",
  "scope_paths": ["cli/src/server/"],
  "detail": "short"
}
```

Docker/server roots often look like `/projects/<repo>`. Host-native may use `/home/<user>/projects/<repo>`. Use a path the **server** can see.

Also available: MCP tools **`butler_orchestrate`**, **`butler_help`**. Prefer **`butler_ask`**.

---

**Contract:** facts only. Prefer structure over search. Follow `next:`. Do not invent callers.

**North star:** faster to trust than grep. Relevant structure only → open those files → edit.

### What Trace *is* (read this before claiming “unused”)

| Fact | Meaning |
|------|---------|
| **CALL edges** | Direct call-expression callees/callers (same-lang). Primary blast surface. |
| **Bridges** | Typed dual-stack (`export` / `ipc` / …) — separate from CALL. |
| **0 CALL callers** | **Not** proof of dead code. Callbacks (`onclick={fn}`), trait objects, DI, routers may leave **zero** CALL edges while still live. |
| **Disambiguate** | Multi-loc seed — pin **exactly one** path from `suggested_scopes` / locations. Prefer a **file** pin. Never bare monorepo-wide `src/` or every-crate `src/lib.rs` (frankenstein merge). |
| **Safe to delete?** | Never from empty callers alone. |

## Tool habit

1. **PRIMARY MCP tool:** `butler_ask` (after this skill is loaded — tool call, not skill args).
2. Not `grep` / `rg` / recursive `list_dir` for structure.
3. Expect **2–3 iterative MCP calls** before “missing.”
4. On miss / disambiguate / BUILDING / empty scope: follow content **`next:`** (or `structured.next_action`).
5. Do not abandon Butler after one empty or partial result.
6. Do not use this skill as a substitute for MCP when tools are available.

Also MCP: `butler_orchestrate` (explicit goal), `butler_help` (contract).

## Multi-pull

| Step | Call | When |
|------|------|------|
| 1 | `mode=arch` + `scope_paths` | Orient package / directory |
| 2 | `symbol=<Ident>` + same `scope_paths` | Callers / callees / edit map / call path |
| 3 | Re-ask with pin from `suggested_scopes` / locations | Disambiguate, miss, incomplete coverage |

**Args:** `project!`, `symbol`?, `mode` ∈ `auto|trace|find|arch|map`, `scope_paths[]`, `detail` ∈ `short|long` (aliases `compact|dense`), `focus_symbol`? (hop continuity), `expand_hops`? ∈ `1|2` (hard cap 2), `sample_offset`?, `exclude_symbols[]`?, `sample_mode` ∈ `score|diverse`.

**Symbol:** Ident/Path only. Prefer `symbol` + `scope_paths` for short names.

### Wrong sample window (200 callers → 10 wrong)

Do **not** dump full reverse. Re-pull with a **different window**:

| Lever | When |
|-------|------|
| `scope_paths` from `suggested_scopes` / `caller_dir_facets` | Blank/wide pin — **first** choice when omitted |
| `sample_offset=N` | Same rank, next slice (banner: `11–20 of 200`) |
| `exclude_symbols=[names in this sample]` | “Not these” |
| `sample_mode=diverse` | Different ranking (dir diversity) |
| `focus_symbol` | You already know the hop parent |

Same args → same sample (memo). Follow compact `next:`.

## Length mode (agent chooses — no mind-reading)

| `detail` | Use when | What you get |
|----------|----------|----------------|
| **short** (default; alias **compact**) | Orient, pin, bridges, mega-hub glance | Trust dossier + **tight** neighbor sample |
| **long** (alias **dense** / full / verbose) | Edit planning under a pin | Full text dump + **larger** neighbor sample |

Honesty is identical (degrees, omitted, mega-hub notes, not-dead-code). Prefer **short first**, then re-ask **same** `symbol` + `scope_paths` with `detail=long` if the sample is thin.

<!-- TODO(optional): unlimited sample only if a specific use case outgrows long under a tight pin; not default. -->

## Read short content (default)

### Arch

| Signal | Action |
|--------|--------|
| `coverage … (complete)` | Use tree; do not list_dir |
| incomplete / rollup | Narrow `scope_paths` from suggestions |
| Scope not found / empty-blocks | Pin suggested scopes; same `project` |

### Trace (edit order)

1. **external callers (cross-file)** — primary edit targets  
2. **local helpers (same-file)** — not external entry points  
3. **trait/boilerplate noise** (`fmt` / `Debug` / `Default` / …) — rarely edit targets  

Also: `receipt: confidence | basis | edges`, bridges (`export`/`ipc`), **`next:`**.

### Hop continuity (`focus_symbol`)

When chaining A→B (Trace A, then Trace B): pass **`focus_symbol=A`** on the B call.

| Fact | Meaning |
|------|---------|
| **What it does** | If A is a real CALL parent of B, Butler force-includes A in the callers **sample** (front of list) |
| **What it does not** | Dump the full hub reverse; raise caps; invent a CALL edge |
| **Miss note** | `focus_symbol not in warehouse callers of ★` → not a parent, or wrong pin |
| **Sample honesty** | Banner still says `callers sample k of warehouse N (omitted …)` — Soft I4 is pack-omit, not missing edge |
| **Multi** | `focus_symbols[]` for several parents; `expand_hops` 1–2 only (hard-capped, not full BFS) |

```text
butler_ask project=<root> symbol=B scope_paths=[…] focus_symbol=A detail=short
```

### Reverse CALL spine (upward edit path)

When present:

```text
call path (reverse spine · CALL only):
  <seed>
  ← parent @ file:line
  ← …
```

| Fact | Meaning |
|------|---------|
| **Direction** | Incoming **CALL** only — toward entry / HTTP surface, not callees |
| **Trust** | Noise wall every hop (`fmt`/`Debug`/`test_*` dropped) |
| **Topology** | Stops at entry (0 product callers) or hub fan-in (>5) — linear spine, not blast tree |
| **Empty** | No tight product pipeline — **do not invent** a stack |
| **Type seeds** | Struct/class → usually **no** spine (`type_neighborhood`); not a call stack |
| **`called by 0` + listed parent** | Warehouse reverse may lag; hop-1 may bootstrap from pack/loc-fallback; say so if scope notes it |

**Use spine for:** “who invokes this toward the top?” / contract change impact on **callers**.  
**Do not use callees list as** the upward path.

### Honesty

- Prefer dossier evidence only. Do not invent callers/files.  
- No external / CALL callers → say so; **do not claim unused, dead, or safe to delete** (callback/IoC/trait may be invisible).  
- Spine missing → say so; do not fabricate `main` / handlers.  
- API change still needs caution even with a clean spine.

## Scope (root-anchored)

- `project` = repo / warehouse root.
- Dir scope = `<project>/<scope>/**` only — not `**/src/**`.
- Suggestions are repo-relative; keep `project` stable on re-ask.

## MCP

Server on `:8002`. Stdio bridge:

```json
{
  "mcpServers": {
    "butler": {
      "command": "/absolute/path/to/lambda-wisperer/target/release/mcp",
      "args": ["--stdio"],
      "env": { "BUTLER_URL": "http://127.0.0.1:8002" }
    }
  }
}
```

Health: `curl -sS http://127.0.0.1:8002/mcp/health`

## Do / don’t

| Do | Don’t |
|----|--------|
| **MCP tool** `butler_ask` for structure | Pass query only to the **skill** and stop |
| Treat skill load as “read the manual” | Treat skill markdown as a graph result |
| Follow `next:` | One empty → filesystem thrash |
| Prefer external + **reverse spine** for caller edits | Treat Debug/Default as entrypoints |
| Root-anchored pins | Assume bare `src/` spans monorepo |
| Read short `content` first | Default to long dump |
| `detail=long` under same pin when sample thin | One global truncation guess |
| Report empty spine honestly | Invent HTTP/router stack |
| Say “MCP not connected” if tool missing | Blame “server offline” after skill-only response |

## Examples

```text
# Edit map (blast / cross-file use)
butler_ask project=<click> mode=arch scope_paths=["src/click/"] detail=short
butler_ask project=<click> symbol=Command scope_paths=["src/click/"] detail=short
→ cite external files from dossier
# If sample thin under pin:
butler_ask project=<click> symbol=Command scope_paths=["src/click/"] detail=long

# Reverse spine (upward path toward entry)
butler_ask project=<wisperer> symbol=handle_orchestrate scope_paths=["cli/"] detail=short
→ read "call path (reverse spine · CALL only)" if present;
  open sole external parent (e.g. dispatch_tool) for contract change

# Mega-hub: pin first, then long under pin
→ next: mega-hub … pin scope_paths; then detail=long same pin

# Hop continuity (A→B without losing A in B's reverse sample)
butler_ask project=<root> symbol=A scope_paths=[…] detail=short
→ pick child B from callees
butler_ask project=<root> symbol=B scope_paths=[…] focus_symbol=A detail=short
→ A should appear at top of callers sample (if real CALL parent)

# Honesty
→ no invent; empty spine / no external → say so; never "safe to delete"
```
