# Uninstall Butler (host install via `install.sh`)

## Stop the server (if running)

```bash
# if you started with butler ui / butler-server:
pkill -f butler-server || true
# confirm port free:
curl -sS http://127.0.0.1:8002/mcp/health || echo "stopped"
```

Docker users: `cd ~/projects/llm-stack && docker compose stop butler` (or `down`) instead — that is a separate install.

## Remove binaries

```bash
rm -f ~/.local/bin/butler \
      ~/.local/bin/butler-server \
      ~/.local/bin/mcp
```

## Optional: config, caches, weights

```bash
# global config
rm -f ~/.config/butler/config.toml
rmdir ~/.config/butler 2>/dev/null || true

# optional weights copied by install.sh
rm -rf ~/.local/share/butler

# per-repo graph caches (only if you want a full wipe)
# find ~ -type d -name .butler 2>/dev/null   # review, then delete chosen trees
```

## Cargo install residue (optional)

If you used `cargo install --path cli` repeatedly:

```bash
# only if you want cargo’s registry copy gone too
cargo uninstall butler 2>/dev/null || true
```

(`cargo install --root ~/.local` primarily drops files under `~/.local/bin`.)

## Re-install

```bash
cd /path/to/butler
./install.sh
# ends with butler ui (server + browser → /setup)
```
