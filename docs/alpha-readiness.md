# Alpha readiness (notes)

> **Public Alpha note:** This file is a snapshot of the map-Alpha bar and gate language.
> Some paths (`plans/receipts/…`, internal dogfood scripts) describe the private lab workflow;
> for strangers, use `scripts/butler_alpha_gate.py`, `docs/ALPHA_SETUP.md`, and `docs/OPS.md`.


**Date:** 2026-07-20 · **re-anchored 2026-07-22** (post peel wave + full dogfood green)  
**Board:** [STACK_STATUS.md](./STACK_STATUS.md) · **Track T:** [agent-desirability.md](./agent-desirability.md)  
**Dual goal:** [dual-goal.md](./dual-goal.md) — this doc is **Goal A (Product Alpha)**. Thesis structure (Goal B) is sequenced, not a substitute for stranger proof — see [context-engine-stage-peel.md](./context-engine-stage-peel.md).  
**Brand note (parked — 2026-07-22):** Repo stays **`lambda-wisperer`**. Product voice = **Butler**. **Trace** = hero verb.  
Optional external brand **Lambda-Atlas** is **not** an Alpha ship item — revisit only at a real **beta** gate (promoted tag / public post we care about), or never. Do **not** rename crates, env (`BUTLER_*`), MCP tools, or Docker for branding. Vanity rename is not a needle.

---

## Snapshot (2026-07-22) — keep it real

Two different scores. Do **not** collapse them.

| Framing | Ready | Meaning |
|---------|-------|---------|
| **Structural-map Alpha** (warm → Trace → honest pack) | **~75–78%** | Late Alpha — careful dogfooders OK; **not** install-and-forget GA |
| **Full-stack Alpha** (gold + neural default + train loop + public shell) | **~45–50%** | Post-map; not the Alpha bar |
| **External brand / docs** | **~35–40%** | A4/A5 one-pagers exist; still eng-voice, not product-marketing ready |
| **Live agent desirability** (open-field “choose Butler over grep”) | **~6.5–7/10** (**~65–70%**) | **Harnessed** A1 Qwen can hit **7/7**; **default structural brain** not proven |

**Quote carefully:**

- **Map machinery:** ~**mid/high 70s** (gates green, peels landed, honesty mill spectacular=0 on tip).  
- **Desirability:** still **specialist**, not default — mid-6 to low-7 fair.  
- **Do not quote “dogfood 7/7 ⇒ product done.”** That is a fixed-task harness with Butler available, not market choice.

Fairness check (unchanged spirit): high-70s engineering + mid-6 desirability is healthy. Gap to **8/10 desirability** = volume headless dogfood on **arbitrary** repos, **guardrails off**, plus evidence agents pick Butler over `rg` **by default**.

### Evidence that re-anchored this board (2026-07-22)

| Signal | Result | Receipt |
|--------|--------|---------|
| Host tests after peels | code_graph **218/218** · orchestrate detail_tests **36/36** | peels P1–P3 |
| Docker rebuild on peeled tip | health ok · fingerprint post-rebuild | Docker butler image |
| Alpha gate A1 | **9/9** lanes · spectacular=**0** · soft≈3 non-blocking | [alpha-gate-latest.json](./receipts/alpha-gate-latest.json) |
| Dogfood accuracy | oracle **6/6** · spectacular=0 · A1 green | [alpha-dogfood-latest.json](./receipts/alpha-dogfood-latest.json) |
| Dogfood agent (Qwen 35b) | **reliability 7/7 = 1.0** on fixed A1 tasks | [desirability_a1_qwen.json](./receipts/desirability_a1_qwen.json) |
| Monster peels | orch/model/context soft stop ≤2k | [monster-file-peel-plan.md](./monster-file-peel-plan.md) |
| A4 / A5 packaging | setup + ops one-pagers | [ALPHA_SETUP.md](./ALPHA_SETUP.md) · [OPS.md](./OPS.md) |

**What 7/7 does *not* prove:** free-form agents without the harness; cold arbitrary monorepos; “opens MCP instead of inventing an `rg` plan” as habit.

---

## What Alpha *is* (ship bar)

> Cold/warm repo → **Trace** is deterministic and honest → agent navigates callers/callees/spine/bridges without inventing edges → watcher does not silently lie after edits → Docker health + fixed gate scripts green on keepers.

### Alpha includes

- Progressive warehouse + serve-while-build  
- Trace packs (short|long), reverse spine, hub UX, disambiguate, file pin  
- Dual-stack Export/IPC floor; integrity honesty  
- Incremental watcher truth (single-file + multi-file cross-file CALL)  
- MCP `butler_ask` + skill surface  
- **Alpha gate suite** (scripts, not a collage of `/tmp` JSON)

### Alpha does *not* require

- Harvester gold mill at scale  
- Neural ranking on by default (Butler Rank stays optional / default-off)  
- Eve training un-stub  
- Public rename of every Butler identifier  
- Cython drawer, unlimited detail mode, OpenHands-scale benchmarks  

### What the product *does* (plain)

1. **Warm** a project  
2. **Trace** a seed (optional pin/scope)  
3. **Read the map pack** (callers, callees, spine, bridges) — not a file tree dump  
4. **Edit with the map**; watcher keeps the graph honest  

**Serves:** map packs (views). **Machinery:** edges/nodes. Pitch: *hands agents the map, not the text.*

---

## Slice breakdown (map Alpha composition — 2026-07-22)

Hand-weighted readiness of **map product**, not a formula. Re-score when evidence moves.

| Slice | ~Ready | Notes (2026-07-22) |
|-------|--------|---------------------|
| Core loop (warm → Trace → pack) | **88%** | T.1–T.3, spine, short\|long, hub, MCP, skill; peels make code operable |
| Warehouse / serve | **85%** | Progressive load, Complete stamp, WarehousePolice |
| Honesty / probes | **85%** | A1 9/9 · spectacular=0 on tip; soft holes residual (non-blocking) |
| Polyglot floor | **75%** | Export/IPC/twin; Cython parked; density enough for product |
| Incremental truth | **80%** | Single + multi batch re-edge + global CALL maps |
| Live agent desirability | **65–70%** | Fixed-harness Qwen **7/7**; open-field default-over-grep **not** closed |
| Harvester gold | **40%** | Pipeline yes; systematic gold + ID alignment open |
| GNN / SmartButler headline | **35–50%** | In-process R-GCN exists; not map-Alpha-critical |
| External product shell | **35–40%** | A4/A5 landed; still eng-first |
| Cross-repo train path | **25%** | Eve stubbed; not map-Alpha blocking |
| Maintainability (monsters) | **✅ wave done** | soft stop ~1.5–2k on orch/model/context · [peel plan](./monster-file-peel-plan.md) |

**Composite map Alpha ~75–78%** = machinery/honesty up; desirability slice still caps “ship confidence.”

---

## Remaining gap — ordered work

| # | Work | Status | Exit signal |
|---|------|--------|-------------|
| **A1** | Alpha gate suite | ✅ | `butler_alpha_gate.py` · spectacular=0 · receipts |
| **A2** | Headless dogfood **volume** loop | 🟡 **pack volume landed** (`butler_desirability_volume.py`); agent volume still A1 set | `python3 -u scripts/butler_desirability_volume.py` · hard cases green · receipt under `plans/receipts/desirability-volume-*.json` |
| **A3** | Live agent loop (operator / free-form) | 🟡 P1 rubric exists; not Track T exit | [desirability-gate-p1.md](./desirability-gate-p1.md) Edit-Map + Honesty green without script hand-holding |
| **A4** | Packaging one-pager | ✅ | [ALPHA_SETUP.md](./ALPHA_SETUP.md) |
| **A5** | Ops polish | ✅ | [OPS.md](./OPS.md) |
| **A6** *(optional)* | One gold harvest showcase | 📋 | Single scoped gold run, not mill |
| **Post-Alpha** | Track B gold mill → Eve retrain → Rank default-on | 🅿️ | Separate full-stack bar |

**Parked for after map Alpha:** OpenHands/SWE-bench solve-rate; Lang MoE; default polyglot AC; Cython; deep rename/Lambda-Atlas crate churn.

---

## Shipping to the door (how “coding → product” actually works here)

Plans and gates are **not** the product. The usual path for a tool like this is a short funnel — most teams skip steps and wonder why “green CI” didn’t ship.

### 1. Name the door (one sentence)

**Map Alpha door:** a careful user can warm a repo, Trace a seed, get an honest pack, and an agent *can* navigate without inventing edges — with fixed scripts proving it on keepers.

**Not the door yet:** “default structural brain for every agent on every repo,” gold mill, or public SaaS.

If the sentence is fuzzy, shipping will thrash between mill, GNN, rename, and peels.

### 2. Ship bar vs wish bar

| Bar | In | Out |
|-----|----|-----|
| **Map Alpha (this door)** | Trace honesty, A1 green, dogfood accuracy, install/ops one-pagers, MCP skill | Gold mill, Rank-on, Eve train, brand rewrite |
| **Desirability 8/10** | Volume harness + free-form agent choice over grep | Optional for first external invite |
| **Full-stack Alpha** | Gold + train loop + neural default | After map door is boring |

**ROI rule:** only spend cycles that move the **named door**. Soft-freeze map edges while peels/docs/harness run; re-open CALL mill only if dogfood spectaculars.

### 3. The loop (repeat until the door is boring)

```text
  build thin slice  →  fixed gate (A1 / dogfood)  →  one human or agent smoke
        ↑                                                    │
        └──────────── fix only what the gate failed ←────────┘
```

- **Unit/detail_tests** = code still compiles and Trace glue didn’t rot.  
- **A1 / spectacular** = packs still honest on keepers.  
- **Dogfood agent** = a real model can *use* the pack under a constrained harness.  
- **Operator smoke** = you (or one dogfooder) still prefer it to `rg` once this week.

Green harness **without** a human still opening the tool is **not** ROI closed.

### 4. Package the door (minimum “out”)

For a local/devtool product, “out the door” is usually:

1. **Binary or image** that someone else can run (Docker compose path exists).  
2. **Three questions answered** (you already have A4): where is Butler home, which program roots, local vs remote server.  
3. **One skill / MCP config** so an agent can call `butler_ask` without lore.  
4. **One command that fails closed** (`butler_alpha_gate.py` / dogfood) so “tip is good” is not tribal knowledge.  
5. **Known limits in writing** (soft holes, warm tax, not RAG) — honesty is part of the product.

You do **not** need a company, a landing page, or a rename to cross map Alpha. Those help **distribution**, not **door definition**.

### 5. First external ROI (smallest real customer loop)

| Step | What “done” looks like |
|------|-------------------------|
| Invite 1–3 careful dogfooders | They warm a **their** repo (or a keeper), Trace 5 seeds, file 0–N trust bugs |
| Capture receipts | Gate JSON + 1 short writeup: “faster than grep?” yes/no/when |
| Fix only trust bugs | Wrong ★, invent, BUILDING lies — not feature requests |
| Repeat until invites stop finding spectaculars | Then desirability work (A2/A3) is the product lever |

**Closed ROI for map Alpha** is not a revenue number. It is: *external person + fixed gate + no spectacular trust break + they come back once.*

### 6. What usually kills “coding to the door”

- Expanding the door mid-flight (gold + GNN + rename + new langs).  
- Treating plan count as progress.  
- Perfecting internal peels while no outsider has run `/warm`.  
- Confusing **harness 7/7** with **market choice**.

### 7. Suggested order from *this* tip (no push — menu)

| If you want… | Next move |
|--------------|-----------|
| **Close map Alpha door** | Freeze scope; invite 1 dogfooder; keep A1 green on tip |
| **Close desirability ROI** | A2 volume harness + A3 operator Edit-Map/Honesty |
| **Close packaging ROI** | Polish skill + README to A4 three questions; optional Lambda-Atlas shell only |
| **Close “self-improving” story** | A6 one gold showcase — after map door is boring |

Default engineer temptation after peels: more peels or more mill. Default product move: **one outsider path + keep the gate green.**

---

## Dogfooding strategy (headless agents)

### Problem

Roo / Cline are excellent **IDE** dogfooders but bad for **CI / batch / arbitrary-repo** loops. Alpha needs **programmatic** volume: many roots, fixed tasks, machine-gradable receipts.

### Options (ranked for *this* stack)

| Rank | Tool | Fit | Verdict |
|------|------|-----|---------|
| **1** | **Custom harness** (HTTP MCP / `butler_ask` + optional `read_file` only) | Full control of system prompt, tool whitelist, JSON logs, asserts (`grep` forbidden, disambiguate handled) | **Primary path.** We already have partial infrastructure (`desirability_a1_qwen.py`, hole/watcher/dogfood probes). Double down into a **nightly desirability unit test**, not a new product dependency. |
| **2** | **Aider** (CLI agent + MCP) | Real headless coding agent; good qualitative transcripts; SWE-bench culture | **Secondary.** Best *drop-in agent* for “does a real agent behave?” after the harness measures structure. Wire to **existing** Butler (`:8002` / MCP), not a fictional Smithery package. Treat Gem’s `npx @smithery/cli run butler-mcp` as a **placeholder** — replace with our real MCP config. |
| **3** | **OpenHands** (dockerized autonomous engineer) | SWE-bench / long-running issues | **Post-Alpha final boss.** Overkill until map Alpha + harness green. |

### Architect stance on Gem’s note

- **Agree:** 72% + ~6.5 desirability is the right honest posture; volume headless dogfood is the bridge to 8/10; Roo alone is not enough; custom harness is how desirability becomes a unit test; OpenHands later.  
- **Agree with caution on Aider-first:** Aider is the right *second* instrument, not the first. CLI agents still digress, call shell/grep, and produce messy transcripts for grading. Use Aider for **human-readable receipts** and the harness for **assertable gates**.  
- **Correct Gem’s wiring:** Butler is already a Docker/local server. Dogfood should hit **our** MCP/HTTP contract (`butler_ask`, project root, scope/pin), not invent a new published MCP package unless we ship one.  
- **Task design (shared by harness + Aider):**  
  - Homonym seeds → force **Disambiguate** / pin  
  - Mega-hubs → **I4 pack-omit honesty** (not “lie by omission”)  
  - Dual-stack seeds → bridges not CALL soup  
  - **No-grep** constraint in prompt + tool allowlist  
  - Keeper set: e.g. `click`, `gin`, word-count/pyo3, wisperer self, one dual-stack keeper  
- **Success receipt for “paradigm” claim:** agent completes a scoped structural task (blast radius / signature edit map) **without** grep/rg as primary structure tool, and handles disambiguate correctly.

### Concrete next steps (dogfood)

1. ~~**Define Alpha gate command**~~ → **A1 landed** (see below).  
2. **Harness v0** — extend existing Qwen/desirability scripts: loop `test_repos` subset; tools = `{butler_ask, read_file}`; log every tool call; assert no grep; score disambiguate / ★ / spine.  
3. **Aider pilot** — 5 tasks × 4 roots; save transcripts under `plans/receipts/` or `/tmp` with dated names; human skim for failure modes harness missed.  
4. **Do not** block Alpha on OpenHands or full SWE-bench.

---

## A1 — Alpha gate suite

**Full dogfood (accuracy + agent) — run this:**

```bash
export LITELLM_MASTER_KEY=…  # your LLM gateway key if using agent dogfood
# Qwen3-35B via LiteLLM with **reasoning OFF**

python3 -u scripts/butler_alpha_dogfood.py
```

Phases: **0 oracle** (gold seeds, no LLM) → **1 A1** (integrity/watcher/holes) → **2 agent** (`desirability_a1_qwen.py`).  
Pack accuracy is first-class: if oracle/A1 fail, agent scores are meaningless.  
Report: `plans/receipts/alpha-dogfood-latest.json`

**Engine only (A1):**

```bash
# tip Butler on :8002
python3 scripts/butler_alpha_gate.py
python3 scripts/butler_alpha_gate.py -v
python3 scripts/butler_alpha_gate.py --lanes health,integrity,watcher
python3 scripts/butler_alpha_gate.py --strict-soft   # soft holes fail too
```

**Accuracy-only smoke (fast):**

```bash
python3 -u scripts/butler_alpha_dogfood.py --only-oracle
```

**Default lanes (order):** `health` → `integrity` → `watcher` → `ffi` → `hole` → `homonym` → `adversarial_gold` → `desirability_curl`

| Lane | Script | Pass rule |
|------|--------|-----------|
| health | `GET /mcp/health` | status=ok |
| integrity | `butler_integrity_gate.py` | all cases pass |
| watcher | `butler_watcher_probe.py --mode all` | spectacular=0 |
| ffi | `butler_ffi_hole_probe.py` | spectacular=0 |
| hole | `butler_hole_probe.py` (wisperer+click) | spectacular=0 |
| homonym | `butler_homonym_hole_probe.py` | spectacular=0 |
| adversarial_gold | `butler_dogfood_adversarial.py --suite gold` | spectacular=0 |
| desirability_curl | `desirability_gate_curl.sh` | exit 0 |

**Policy:** soft holes do **not** fail the gate unless `--strict-soft`.  
**Reports:** `plans/receipts/alpha-gate-latest.json` + stamped `alpha-gate-<utc>.json` + per-lane under `plans/receipts/alpha-gate-lanes/`.

**Keepers (host):** word-count, click, gin, wisperer, pybind11 under `$BUTLER_HOST_MOUNT` (default `$BUTLER_HOST_MOUNT` or `$HOME/projects`).

### Product contract (do not over-claim green dogfood)

| Trace proves | Trace does **not** prove |
|--------------|---------------------------|
| CALL callers/callees (+ export/ipc bridges) | All “usage” (callbacks, trait objects, DI, routers) |
| Multi-loc → disambiguate / pin | Safe to delete when callers empty |
| Honest empty / incomplete | Framework magic wiring |

**0 CALL callers ≠ dead code.** Semantic traps T7/T9 in desirability exercise agent honesty; oracle O5/O6 lock engine boundaries.

### Semantic boundary tasks (agent)

| ID | Trap | Pass |
|----|------|------|
| **T7** | `log` @ tauri Communication — `onclick={log}` | Seed OK; **no** “dead/never called”; acknowledge 0 CALL / callback |
| **T9** | multi-loc `parse` @ wisperer | Disambiguate → pin **one file**; no frankenstein merge claim |

```bash
# traps only (after baseline green):
python3 -u scripts/desirability_a1_qwen.py --only T7,T9
```

### Last green

| Date | Fingerprint | Result | Note |
|------|-------------|--------|------|
| 2026-07-20 | `butler-2a968935785f41ca9b07d454a31f2f3c` | **Dogfood full:** oracle 4/4 · A1 8/8 · agent **5/5** · ~158s | [alpha-dogfood receipt](./receipts/alpha-dogfood-latest.json); soft=2 non-blocking |
| 2026-07-20 | `butler-f01e073232e24abd915ba4c8d23703fe` | A1 8/8 · spectacular=0 soft=2 · ~20s | engine-only baseline |

---

## Naming (parked)

| Layer | Name | Status |
|-------|------|--------|
| Repo / git | `lambda-wisperer` | **Keep** through Alpha (and fine forever) |
| Product / process / tools | **Butler** | Canonical voice |
| Hero verb | **Trace** | Keep |
| External brand “Lambda-Atlas” | optional map metaphor | **Parked** — beta-or-never; not a ship blocker |

Pitch line (Alpha):

> **Butler** — structural maps for coding agents. **Trace** a symbol; get a deterministic neighborhood — not context stuffing.

If beta ever wants a public face: *Lambda-Atlas (Butler)* externally only — still no crate/env rename without a separate decision.

---

## Related

| Doc | Role |
|-----|------|
| [STACK_STATUS.md](./STACK_STATUS.md) | Living board |
| [agent-desirability.md](./agent-desirability.md) | Track T deep plan |
| [desirability-gate-p1.md](./desirability-gate-p1.md) | Edit-Map + Honesty rubric |
| [integrity-gate.md](./integrity-gate.md) | Structural integrity scripts |
| [qwen-bakeoff-v10.md](./qwen-bakeoff-v10.md) | Bake-off framing |

---

## Changelog

| Date | Note |
|------|------|
| 2026-07-22 | **Brand parked:** repo stays `lambda-wisperer`; Butler + Trace; Lambda-Atlas = beta-or-never, not Alpha needle |
| 2026-07-22 | **Re-anchor keep-it-real:** map Alpha ~**75–78%** (not 72); desirability still ~**6.5–7/10** specialist; harnessed Qwen **7/7** ≠ default-over-grep; A1/A4/A5/peels/dogfood evidence table; **Shipping to the door** process section |
| 2026-07-20 | Initial Alpha readiness (~72% map Alpha); gap A1–A6; dogfood harness-primary + Aider-secondary; naming Atlas external / Butler internal |
| 2026-07-20 | **A1:** `scripts/butler_alpha_gate.py` — fixed multi-lane gate + receipts |
| 2026-07-20 | Full dogfood green 5/5 + contract + T7/T9 semantic boundary tasks |
