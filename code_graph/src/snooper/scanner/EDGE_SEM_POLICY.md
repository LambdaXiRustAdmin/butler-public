# Edge semantics + Complete cache reliability

**Audience:** anyone changing CALL / bridge / linker / interconnect behavior.  
**Live constants:** `cache.rs` → `GRAPH_SCHEMA_VERSION`, `EDGE_SEMANTICS_VERSION`.

---

## Dual versioning (do not collapse)

| Constant | Bump when | Mismatch effect |
|----------|-----------|-----------------|
| **`GRAPH_SCHEMA_VERSION`** | On-disk **layout** of nodes/records changes (serde shape, required fields, inventory membership policy that invalidates stored nodes) | Shards ignored / full rescan |
| **`EDGE_SEMANTICS_VERSION`** | **Who is connected to whom** would change under new edge logic, with layout unchanged | **Keep nodes + file_hashes**, **drop edges + bridges**, stamp Incomplete → FullEdge rebuild |

Honesty: **never serve Complete edges built under a different edge_sem.**

### Operator note — product definition-tier grain (GRAPH_SCHEMA 15)

**Product inventory is definition-tier only** (functions/methods/classes/structs/impls/traits/mods + export-surface declarators). Statement/expression AST kinds are **not** permanent warehouse nodes. FullEdge still queries the AST for call sites.

| Situation | Action |
|-----------|--------|
| Existing Complete cache (schema ≤14) | **Invalid for product** — load path full-rescans (schema mismatch). Delete `.butler/cache` if you want a clean rebuild without partial thrash. |
| After upgrade to schema 15 | `butler warm --full` / first open triggers rescan + FullEdge. Do not copy old-grain shards across hosts. |
| Training-rich graphs | **Out of product path** — dedicated offline builder later (Hop B). |

Do **not** mix statement-grain nodes with definition-tier edges and call the result Complete.

---

## When to bump `EDGE_SEMANTICS_VERSION`

**Bump if** a stored edge list would be **wrong or incomplete** after your change:

- Call resolution (function-like only, QueryOnly vs body-scan, same-lang maps)
- Import / barrel / path-alias resolution that adds/removes CALL edges
- Typed bridges (Export / Ipc / Twin) rules or hosts
- FFI attach precision (false positives/negatives)
- Defaults that change edge **population** (e.g. polyglot AC on/off as product default)

**Do not bump for:**

- Pure refactors with identical edge output
- Logs, Trace pack ranking, presentation, next_action
- Agent skill / MCP ceremony
- Performance-only changes that do not change edges produced

**Who:** the author of the edge-logic change, in the **same commit** as the behavior change.  
**How:** increment const + one-line comment in the bump log on `EDGE_SEMANTICS_VERSION`.

There is **no** automatic “this PR needs a bump” detector. Discipline only.

---

## Mismatch policy (no migrators by default)

```text
disk edge_sem == live  →  may trust Complete (after fingerprint)
disk edge_sem != live  →  drop edges/reverse/bridges; keep skeleton; Incomplete
```

No stepwise migrators (v28→v31). No “compatible range.”  
A narrow one-step migrator is **Hop B** only with measured justification (not default).

Legacy caches missing `edge_semantics` deserialize as **0** → always stale vs current.

---

## Trusted Complete reopen (happy path)

```text
load_shards
  → Complete + edges non-empty
  → sources_fp.bin matches inventory stat fingerprint
       (sources_stat_fingerprint_from_inventory — same keys as save)
  → skip content-hash
  → finalize_loaded_graph_state (trusted_complete skips O(nodes) canonize)
```

**False-miss class (fixed):** walk-based fingerprint vs inventory stamp used different path sets → content-hash / FullEdge thrash.  
**Fix:** match **inventory keys first** (save and load); walk is fallback only.

Timers to inspect: `Loaded shards…`, `Hydrate trusted Complete…`, `Content-hash verify…`, `Load finalize…`.

---

## Save failures (must not poison forever)

If FullEdge completes in RAM but **cannot write** `.butler/cache` (common: **root-owned** Docker litter):

- Disk still has old edge_sem Complete
- Next open: drop edges → rebuild → fail save → loop

**Hardening (Hop A):**

- `probe_butler_cache_dir_writable` before ensure cache / on edge_sem drop / FullEdge preflight
- `save_shards` errors **propagated** (no silent `let _ =`)
- PermissionDenied logs **chown remediation**
- Operator fix: `chown -R "$(whoami)" /path/to/repo/.butler` then re-warm

---

## Recent edge_sem inventory (v29–v31) — could any be avoided?

| Ver | Change (from const log) | FullEdge justified? |
|-----|-------------------------|---------------------|
| **29** | Rust CALL QueryOnly (no aggressive body-scan) | **Yes** — removes false CALL edges |
| **30** | IPC line-span filter + invoker rank | **Yes** — changes who is linked |
| **31** | pybind Export never targets TEST_SUBMODULE / junk hosts | **Yes** — drops bad bridges |

None of v29–v31 look “pure additive keep-old-edges safe.” Keeping old edges would serve **polluted** CALL/bridge maps under new honesty rules. **No Hop B migrator** recommended for these.

Older bumps (19 bridges, 17–28 TS import/barrel, 12–15 FFI precision) same class: behavior deltas, not cosmetic.

---

## Operator checklist after a real edge_sem bump

1. Bump const + comment in the same PR as the logic change.  
2. Expect first warm of each repo to **drop edges + FullEdge** (minutes on leviathans).  
3. Ensure `.butler/cache` is **user-writable** so Complete **persists**.  
4. Second warm should hit **Hydrate trusted Complete** (no content-hash if tree unchanged).
