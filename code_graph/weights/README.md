# Butler GNN weights (SmartButler)

Trained parameter blobs for **in-process** relevance scoring in `code_graph/src/gnn/`.

Training still runs in **Eve → Xi**; the drop format is a raw little-endian `f32` vector
(historically Eve wrote this under `.butler/weights/`). Butler is now the home for the
canonical inference copy and the load path.

## Layout

```
code_graph/weights/
  README.md                 # this file
  MANIFEST.toml             # provenance for the active default
  gnn_trained_global.bin    # active default used for inference
  checkpoints/              # optional epoch ladder (gitignored; keep local)
```

| Path | Role |
|------|------|
| `gnn_trained_global.bin` | **Active default** checked into the repo (~3 MiB, 786 432 f32s — full trinity-sized blob; forward uses the L1/L2 prefix). |
| `checkpoints/` | Optional `gnn_trained_global_e{N}.bin` from training runs. Not required at runtime. |
| `{project}/.butler/weights/` | Per-project override / Eve training drop-in (gitignored via `.butler/`). |
| `~/.local/share/butler/weights/` | Copy installed by `install.sh` for non-source-tree runs. |

## Load resolution (`load_weights`)

Highest priority first:

1. `BUTLER_GNN_WEIGHTS` — absolute path to a `.bin` file  
2. `{project_root}/.butler/weights/gnn_trained_global.bin`  
3. `~/.local/share/butler/weights/gnn_trained_global.bin`  
4. Crate-relative `code_graph/weights/gnn_trained_global.bin` (`CARGO_MANIFEST_DIR`)  
5. Cwd-relative `code_graph/weights/gnn_trained_global.bin`  
6. Synthetic tiny init (untrained fallback — scores will be weak)

## Format

- **File**: raw LE `f32` stream, no header  
- **Expected size (current training dumps)**: 3 145 728 bytes = 786 432 floats  
- **Consumer**: `cpu_gnn_forward` reads L1 (`NUM_REL * FEATURE_DIM * HIDDEN`) then L2 (`HIDDEN`); extra floats are ignored for scoring  
- **FEATURE_DIM** = 32, **HIDDEN** = 64, **NUM_REL** = 5 (see `src/gnn/`)

## Dual-publish (Eve training → both repos)

**Preferred:** Eve owns the write. Final `gnn_trained_global.bin` is fanned out to:

| Destination | Purpose |
|-------------|---------|
| `<training-workspace>/.butler/weights/` | Training workspace / resume |
| `lambda-wisperer/code_graph/weights/` | Butler source of truth (this tree) |
| `lambda-wisperer/.butler/weights/` | Local project override for self-scoring |
| `~/.local/share/butler/weights/` | Installed runtime |

```bash
# After training (or anytime the Eve blob is newer):
cd /path/to/training-workspace
cargo run -- --publish-gnn-weights
# or: ./scripts/publish_gnn_global.sh

# Butler-side pull if you only have a shell here:
./scripts/sync_gnn_weights_from_eve.sh
```

Env overrides: `BUTLER_ROOT` / `LAMBDA_WHISPERER`, `EVE_ROOT`, optional `--weights-src PATH`.

Epoch ladder files (`gnn_trained_global_e{N}.bin`) stay **Eve-local** under `.butler/weights/`.
Only the final **global** is dual-published.

Optional: keep an epoch snapshot under `checkpoints/` (gitignored) for A/B:

```bash
cp /path/to/<training-workspace>/.butler/weights/gnn_trained_global_e50.bin \
   code_graph/weights/checkpoints/
```

Update `MANIFEST.toml` when you intentionally promote a new global to git.

## Related code

- `code_graph/src/gnn/projection.rs` — `load_weights`, `weight_search_paths`, feature build  
- `code_graph/src/gnn/forward.rs` — CPU R-GCN  
- `cli/src/server/neural.rs` — SmartButler scoring hook  
