Butler — portable Windows x64
==============================

Contents (keep together in one folder):
  butler.exe
  butler-server.exe
  mcp.exe
  weights\   (optional GNN weights)

First run
---------
  1. Unzip anywhere (e.g. Downloads\Butler or %LOCALAPPDATA%\Butler)
  2. Double-click nothing required — open a terminal in this folder:
       butler.exe ui
  3. Browser opens http://127.0.0.1:8002/setup
     Green light = engine alive
     Copy the MCP JSON into Cursor / Cline / Roo
  4. Leave the server running while agents use Butler

Manual server (if needed)
-------------------------
  butler-server.exe
  then open http://127.0.0.1:8002/setup

Operator tools (export / harvest)
---------------------------------
  http://127.0.0.1:8002/ops

Notes
-----
  - All three .exe files must stay in the same directory (butler ui spawns butler-server.exe by path).
  - Default bind is localhost only.
  - SmartScreen may warn on unsigned builds — More info → Run anyway for private Alpha.
  - No Docker Desktop required.
