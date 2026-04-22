# Run all recipes through mise so managed tools (trunk, wrangler, jq, etc.)
# and env vars (RUSTFLAGS) are always available — no manual activation needed.
set shell := ["mise", "x", "--", "bash", "-c"]

# Default recipe.
default: dev-demo

# ── Setup ─────────────────────────────────────────────────────────────────────

# Install all tools and compile targets (run once after cloning).
setup:
  mise install
  rustup target add wasm32-unknown-unknown

# Normal dev run. The most used command.
dev-demo:
  cargo run --package=pmetra_demo --features=dev

# Run dev with tracy (`bevy/trace_tracy`). This is useful for profiling.
dev-demo-tracy:
  cargo run --package=pmetra_demo --features=dev,bevy/trace_tracy

# Build the release version of the pmetra demo.
build-release-demo:
  cargo build --package=pmetra_demo --release

# Build all.
build:
  cargo build

# Build and serve the pmetra demo web (localhost).
build-serve-pmetra-demo: build-pmetra-demo-web trunk-serve-web

# Build the web release version of the pmetra demo.
build-pmetra-demo-web:
  trunk build --release --no-default-features
  # Strip trunk's live-reload script (fails on Cloudflare, not needed in production).
  sed -i '' '/<script>"use strict";/,/<\/script><\/body>/d' dist/index.html
  echo '</body>' >> dist/index.html
  # Strip integrity="..." attrs — wasm-opt (below) rewrites the .wasm so the SRI
  # hashes computed by trunk would no longer match, blocking the script in-browser.
  sed -i '' 's/ integrity="[^"]*"//g' dist/index.html
  # Size-optimize WASM with system binaryen (newer than trunk's bundled copy,
  # handles Bevy's modern WASM features).
  for f in dist/*_bg.wasm; do \
    orig=$(du -h "$f" | cut -f1); \
    wasm-opt -Oz --enable-bulk-memory --enable-reference-types --enable-sign-ext \
      --enable-mutable-globals --enable-nontrapping-float-to-int \
      --enable-multivalue --enable-simd --enable-bulk-memory-opt \
      -o "$f.opt" "$f" && mv "$f.opt" "$f"; \
    echo "  wasm-opt: $(basename $f): $orig -> $(du -h $f | cut -f1)"; \
  done

# Serve WASM app (localhost, with live reload). Port 3000 is the standard
# local dev port — the WASM bridge auto-detects it and connects to ws://:9001.
trunk-serve-web:
  trunk serve --release --no-default-features --port 3000

# FAST local dev loop. Skips wasm-opt + fat LTO + size optimization —
# builds in ~20s instead of ~3 min. Use for iterating on Rust/CSS/HTML
# changes. NEVER use this output for shipping — bundle is huge and slow.
dev-web:
  #!/usr/bin/env -S mise x -- bash
  export CARGO_PROFILE_RELEASE_LTO=off
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
  export CARGO_PROFILE_RELEASE_OPT_LEVEL=1
  export CARGO_PROFILE_RELEASE_STRIP=none
  trunk serve --release --no-default-features --port 3000

# Same fast profile but binds 0.0.0.0 so your phone can hit http://<LAN-IP>:3000
# to test mobile touch / handle interactions without re-uploading to CF.
dev-web-lan:
  #!/usr/bin/env -S mise x -- bash
  export CARGO_PROFILE_RELEASE_LTO=off
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
  export CARGO_PROFILE_RELEASE_OPT_LEVEL=1
  export CARGO_PROFILE_RELEASE_STRIP=none
  IP=$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || echo "localhost")
  echo ""
  echo "  Phone URL: http://${IP}:3000"
  echo ""
  trunk serve --release --no-default-features --address 0.0.0.0 --port 3000

# Serve WASM app on LAN (accessible from phone/tablet on same WiFi).
# Add ?model=<variant> to URL to select initial model.
serve-lan:
  #!/usr/bin/env -S mise x -- bash
  IP=$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || echo "localhost")
  echo ""
  echo "  ── Demo URLs (open on phone/desktop, same WiFi) ──"
  echo ""
  echo "  Default (2 towers):  http://${IP}:3000"
  echo "  NURBS surface:       http://${IP}:3000?model=ExpNurbsSolid"
  echo "  Tower extension:     http://${IP}:3000?model=TowerExtension"
  echo "  Round cabin:         http://${IP}:3000?model=RoundCabinSegment"
  echo "  Cube + cylinder:     http://${IP}:3000?model=SimplCubeAtCylinder"
  echo "  Cube + tower:        http://${IP}:3000?model=MultiModelsSimplCubeAtCylinderAndTowerExtension"
  echo "  2x towers:           http://${IP}:3000?model=MultiModels2TowerExtensions"
  echo ""
  echo "  Touch: 1-finger=orbit, 2-finger=pan, pinch=zoom, tap=select"
  echo ""
  trunk serve --release --no-default-features --address 0.0.0.0 --port 3000

# Build WASM + serve on LAN in one step.
build-serve-lan: build-pmetra-demo-web serve-lan

# ── MCP server ────────────────────────────────────────────────────────────────

# Start the Rust MCP server (stdio transport — use with claude CLI or Desktop).
mcp-rs:
  cargo run -p pmetra-mcp-server

# Run the Rust MCP server as a WS-only broker (no MCP stdio client needed).
# WASM app connects to ws://localhost:9001. Useful for Playwright / JS API testing.
mcp-broker:
  cargo run -p pmetra-mcp-server < /dev/null

# Build the Rust MCP server (dev, no embedded UI).
build-mcp-rs:
  cargo build -p pmetra-mcp-server --release

# Build the all-in-one binary: MCP server + embedded WASM app.
# End users download this one file, run it, open http://localhost:9001.
# Requires: dist/ exists (run build-pmetra-demo-web first).
build-release: build-pmetra-demo-web
  cargo build -p pmetra-mcp-server --release --features embed-ui
  @ls -lh target/release/pmetra-mcp-server
  @echo ""
  @echo "  Binary: target/release/pmetra-mcp-server"
  @echo "  Run:    ./target/release/pmetra-mcp-server"
  @echo "  Open:   http://localhost:9001"

# ── Dev workflow ─────────────────────────────────────────────────────────────

# Start everything for local dev: WS broker (background) + trunk serve (foreground).
# WASM auto-reconnects to the broker, so you can restart either side independently.
# Ctrl+C stops trunk; run `just stop` to clean up the broker.
# Open http://127.0.0.1:3000?model=TowerExtension in your browser.
dev-all:
  #!/usr/bin/env -S mise x -- bash
  just stop 2>/dev/null
  echo "Starting WS broker (port 9001)..."
  nohup cargo run -p pmetra-mcp-server < /dev/null > /tmp/pmetra-mcp.log 2>&1 &
  echo $! > /tmp/pmetra-mcp.pid
  export CARGO_PROFILE_RELEASE_LTO=off
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
  export CARGO_PROFILE_RELEASE_OPT_LEVEL=1
  export CARGO_PROFILE_RELEASE_STRIP=none
  echo ""
  echo "  WS broker:  ws://127.0.0.1:9001  (PID $(cat /tmp/pmetra-mcp.pid))"
  echo "  HTTP:       http://127.0.0.1:3000 (trunk serve, below)"
  echo "  MCP log:    /tmp/pmetra-mcp.log"
  echo "  Stop broker: just stop"
  echo "  Smoke test:  just test-bridge"
  echo ""
  trunk serve --release --no-default-features --port 3000

# Stop the background WS broker.
stop:
  #!/usr/bin/env -S mise x -- bash
  for pidfile in /tmp/pmetra-mcp.pid; do
    if [ -f "$pidfile" ]; then
      kill "$(cat "$pidfile")" 2>/dev/null
      rm -f "$pidfile"
    fi
  done
  lsof -i :9001 -t 2>/dev/null | xargs kill 2>/dev/null || true
  echo "Stopped."

# ── Tests ─────────────────────────────────────────────────────────────────────

# Run all tests.
test:
  cargo test

# Smoke test the running bridge. Requires: just dev-all + browser open on :3000.
# Uses the HTTP API — no pipes, no second process, no disconnecting the WASM app.
test-bridge:
  #!/usr/bin/env -S mise x -- bash
  set -e
  echo "=== Health ==="
  curl -sf localhost:9001/health | jq .
  echo ""
  echo "=== Schema: TowerExtension ==="
  REPLY=$(curl -sf -X POST localhost:9001/call -H 'Content-Type: application/json' \
    -d '{"cmd":"schema","name":"TowerExtension"}')
  echo "$REPLY" | jq .
  echo "$REPLY" | jq -e '.value.fields[] | select(.name == "tower_length")' > /dev/null \
    && echo "OK" || { echo "FAIL: tower_length not found in schema"; exit 1; }
  echo ""
  echo "=== List resources ==="
  curl -sf -X POST localhost:9001/call -H 'Content-Type: application/json' \
    -d '{"cmd":"list"}' | jq -r '"  \(.value | length) resources", (.value[:10][] | "  \(.)")'

# ── Cloudflare deployment ─────────────────────────────────────────────────────
#
# The CF Worker (workers/worker.js) does two things:
#   1. Serves the WASM app from R2 (static files)
#   2. Relays MCP commands via Durable Object WebSocket broker
#
# Same /health and /call API as the local Rust MCP server — tests work on both.
# WASM app auto-detects CF (HTTPS → wss://<host>/ws) vs local (HTTP → ws://<host>:9001).

# First-time setup: create R2 bucket + deploy DO migration (run once).
cf-init:
  wrangler r2 bucket create pmetra-assets

# Upload dist/ files to R2 and deploy the Worker + Durable Object.
cf-deploy: build-pmetra-demo-web cf-upload cf-worker

# Upload all dist/ files to R2 bucket (skips .stage/).
cf-upload:
  #!/usr/bin/env -S mise x -- bash
  echo "Uploading dist/ to R2 bucket pmetra-assets..."
  cd dist
  find . -type f -not -path './.stage/*' | while read -r file; do
    key="${file#./}"
    echo "  $key"
    wrangler r2 object put "pmetra-assets/$key" --file "$file" --remote
  done
  echo "Done."

# Deploy the Worker + Durable Object to Cloudflare.
cf-worker:
  wrangler deploy

# Run the Worker locally against the REMOTE R2 bucket + real DO runtime.
# WASM app at http://localhost:8787 auto-connects to ws://localhost:8787/ws.
# Test with: just cf-test 8787
cf-dev:
  wrangler dev --remote

# Build + upload + run locally in full CF parity mode.
cf-dev-build: build-pmetra-demo-web cf-upload cf-dev

# Smoke test the CF Worker (local or remote). Default port: 8787 (wrangler dev).
# Usage: just cf-test          → test localhost:8787
#        just cf-test 443 pmetra-demo.gedw99.workers.dev https
cf-test port="8787" host="localhost" scheme="http":
  #!/usr/bin/env -S mise x -- bash
  set -e
  BASE="{{scheme}}://{{host}}:{{port}}"
  if [ "{{port}}" = "443" ]; then BASE="{{scheme}}://{{host}}"; fi
  echo "Testing $BASE ..."
  echo ""
  echo "=== Health ==="
  curl -sf "$BASE/health" | jq .
  echo ""
  echo "=== Schema: TowerExtension ==="
  REPLY=$(curl -sf -X POST "$BASE/call" -H 'Content-Type: application/json' \
    -d '{"cmd":"schema","name":"TowerExtension"}')
  echo "$REPLY" | jq .
  echo "$REPLY" | jq -e '.value.fields[] | select(.name == "tower_length")' > /dev/null \
    && echo "OK" || { echo "FAIL: tower_length not found in schema"; exit 1; }
  echo ""
  echo "=== List resources ==="
  curl -sf -X POST "$BASE/call" -H 'Content-Type: application/json' \
    -d '{"cmd":"list"}' | jq -r '"  \(.value | length) resources", (.value[:10][] | "  \(.)")'

# Print live demo URLs (Cloudflare).
cf-urls:
  @echo ""
  @echo "  ── Live Demo URLs (Cloudflare) ──"
  @echo ""
  @echo "  Default (2 towers):  https://pmetra-demo.gedw99.workers.dev"
  @echo "  NURBS surface:       https://pmetra-demo.gedw99.workers.dev?model=ExpNurbsSolid"
  @echo "  Tower extension:     https://pmetra-demo.gedw99.workers.dev?model=TowerExtension"
  @echo "  Round cabin:         https://pmetra-demo.gedw99.workers.dev?model=RoundCabinSegment"
  @echo "  Cube + cylinder:     https://pmetra-demo.gedw99.workers.dev?model=SimplCubeAtCylinder"
  @echo "  Cube + tower:        https://pmetra-demo.gedw99.workers.dev?model=MultiModelsSimplCubeAtCylinderAndTowerExtension"
  @echo "  2x towers:           https://pmetra-demo.gedw99.workers.dev?model=MultiModels2TowerExtensions"
  @echo ""

# List all available recipes.
list:
  just --list
