#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="${HOME}/.local/bin"
# Build from workspace root so -p cli resolves.
cd "$ROOT"

echo "Installing Butler to ${BIN} ..."
echo "(building release binaries — this can take a minute the first time)"
echo ""

# One release build; copy bins (avoids cargo install re-resolving crates.io noise).
cargo build --release -p cli

mkdir -p "$BIN"
install -m 755 "$ROOT/target/release/butler" "$BIN/butler"
install -m 755 "$ROOT/target/release/butler-server" "$BIN/butler-server"
install -m 755 "$ROOT/target/release/mcp" "$BIN/mcp"

# Optional weights (neural scoring stays off for Alpha)
WEIGHTS_SRC="$ROOT/code_graph/weights/gnn_trained_global.bin"
WEIGHTS_DST="$HOME/.local/share/butler/weights"
if [[ -f "$WEIGHTS_SRC" ]]; then
  mkdir -p "$WEIGHTS_DST"
  cp -f "$WEIGHTS_SRC" "$WEIGHTS_DST/gnn_trained_global.bin"
fi

echo ""
echo "Installed:"
echo "  ${BIN}/butler"
echo "  ${BIN}/butler-server"
echo "  ${BIN}/mcp"
echo ""
echo "Neural / GNN scoring: disabled for Alpha."
echo ""
echo "Starting server with this install and opening setup…"
echo "  http://127.0.0.1:8002/setup"
echo "  (uses --restart so a stale butler-server is replaced)"
echo ""

# --restart: stop old host butler-server so new /setup HTML is served.
# Install still succeeds if browser open fails.
if ! "${BIN}/butler" ui --restart; then
  echo ""
  echo "Launch had a problem. Try:"
  echo "  pkill -f butler-server || true"
  echo "  ${BIN}/butler ui --restart"
  echo "Then open http://127.0.0.1:8002/setup"
  echo "If Docker owns :8002:  stop any other process using :8002 (e.g. your docker compose butler service)"
  exit 0
fi

echo ""
echo "Done. If the browser did not open, go to: http://127.0.0.1:8002/setup"
echo "Uninstall: packaging/UNINSTALL.md"
echo "PATH tip: export PATH=\"\$HOME/.local/bin:\$PATH\""
echo ""
