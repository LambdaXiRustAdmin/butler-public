---
name: butler-ask
description: >
  Before you edit a function: who_calls (direct callers/callees). Wiring / unused? / rewire
  → who_calls first. grep forbidden for that question. rg OK for rename-all-text / string find.
  After reading, call MCP who_calls (internal alias butler_ask). Not this skill again.
  Triggers: who calls, who_calls, unused, rewire, blast radius, callers, callees, butler, Trace.
---

# who_calls (Butler)

## Gate

Do not patch a wiring-shaped edit until a **primary pack** (or honest empty) for this symbol.

Wiring-shaped = signature / visibility / unused? / rewire / post-edit.

If the host cannot hard-block grep, **order** who_calls first anyway.

## Door

MCP tool **`who_calls`**. Internal alias: **`butler_ask`**. Same args. `butler_orchestrate` is the power tool.

This skill is documentation. **Call the MCP tool.** Skill markdown is not a pack.

| If | Then |
|----|------|
| `who_calls` / `butler_ask` in the tool list | Call it |
| Tool missing | “Butler MCP not connected” |
| Server down / warehouse not ready | `infra-skip` — do **not** pretend the skill failed the map |

```json
{
  "project": "/ABS/PATH/TO/REPO",
  "symbol": "theFunction",
  "scope_paths": ["src/"],
  "detail": "short"
}
```

## Promise (buy line)

Direct CALL callers/callees for this symbol (**same language**).

- Not every textual hit.
- Not hop-2. Hop-2 is not a caller.
- Not guaranteed cross-FFI.
- **Go methods** often show `0` reverse CALL while real calls exist — not “Go is off.”
- **0 direct ≠ unused.** Callbacks, DI, trait objects, routers may be invisible.

Do not ship “you’ll never miss a caller.”

## Grep

| Question | Tool |
|----------|------|
| who-calls / unused? / rewire | **who_calls first.** grep **forbidden**. |
| rename-all-text / string / error / config key | rg OK |

Do not abandon the pack for bash after one empty. Follow content `next:`. Expect 2–3 calls (orient / pin / re-Trace). `detail=long` under the same pin for the full primary list.

## After the pack

1. Read the pain headline: direct callers / callees + N total + hop disclaimer.
2. Edit the **external** CALL files first.
3. Disambiguate: pin **one** path from `locations` / `suggested_scopes`.
4. Never delete from empty callers alone.

**Hold** if you are in this tree all session:

```bash
butler hold -r <abs-project-root> --server http://127.0.0.1:8002
```

Health: `curl -sS http://127.0.0.1:8002/mcp/health`
