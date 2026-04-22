# Run all recipes through mise so managed tools (trunk, wrangler, bun, etc.)
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

# Serve WASM app (localhost, with live reload).
trunk-serve-web:
  trunk serve --release --no-default-features

# FAST local dev loop. Skips wasm-opt + fat LTO + size optimization —
# builds in ~20s instead of ~3 min. Use for iterating on Rust/CSS/HTML
# changes. NEVER use this output for shipping — bundle is huge and slow.
dev-web:
  #!/usr/bin/env -S mise x -- bash
  export CARGO_PROFILE_RELEASE_LTO=off
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
  export CARGO_PROFILE_RELEASE_OPT_LEVEL=1
  export CARGO_PROFILE_RELEASE_STRIP=none
  trunk serve --release --no-default-features

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

# ── MCP servers ───────────────────────────────────────────────────────────────

# Start the TypeScript MCP server (requires: bun, node_modules installed).
mcp-ts:
  bun run mcp-server/index.ts

# Start the Rust MCP server (stdio transport — use with claude CLI or Desktop).
mcp-rs:
  cargo run -p pmetra-mcp-server

# Build the Rust MCP server release binary.
build-mcp-rs:
  cargo build -p pmetra-mcp-server --release

# ── Tests ─────────────────────────────────────────────────────────────────────

# Run all tests.
test:
  cargo test

# ── Cloudflare deployment ─────────────────────────────────────────────────────

# First-time setup: create R2 bucket (run once).
cf-init:
  wrangler r2 bucket create pmetra-assets

# Upload dist/ files to R2 and deploy the Worker.
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

# Deploy the Worker that serves from R2.
cf-worker:
  wrangler deploy

# Run the Worker locally against the REMOTE R2 bucket — same code path as prod.
# Catches Worker/cache/encoding bugs before deploying. Serves on http://localhost:8787.
cf-dev:
  wrangler dev --remote

# Build + run locally in Worker/R2 parity mode. Uploads dist/ to R2 first.
cf-dev-build: build-pmetra-demo-web cf-upload cf-dev

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
