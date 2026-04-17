# Default recipe.
default: dev-demo

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
  #!/bin/bash

  echo "trunk build in release mode..."
  RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk build --release --no-default-features
  echo "trunk build in release mode... done!"

# Serve WASM app (localhost, with live reload).
trunk-serve-web:
  RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk serve --release --no-default-features

# Serve WASM app on LAN (accessible from phone/tablet on same WiFi).
# Add ?model=<variant> to URL to select initial model.
serve-lan:
  #!/bin/bash
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
  RUSTFLAGS="--cfg=web_sys_unstable_apis" trunk serve --release --no-default-features --address 0.0.0.0 --port 3000

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

# Build for Vercel deployment.
vercel-build: build-pmetra-demo-web
  vercel build

# Build and deploy demo WASM via Vercel.
vercel-deploy: vercel-build
  vercel deploy --prebuilt

# List all available recipes.
list:
  just --list

# This is a comment.
example-recipe:
  @echo 'This is example recipe.'
