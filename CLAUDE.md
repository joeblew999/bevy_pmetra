# bevy_pmetra — AI Session Context

## Objective

Build a system where an AI (Claude) can fully control a parametric CAD application running
in a browser, with no human in the loop. The goal is AI-driven CAD: describe what you want,
the geometry changes.

```
user / AI intent
      ↓
MCP server (Rust, mcp-server-rs/)
      ↓ WebSocket (port 9001)
WASM bridge (wasm_bridge.rs)
      ↓ Bevy ECS reflection
CAD geometry rebuilds
      ↓
screenshot() confirms result
```

The web delivery is intentional — it is the only way AI has both **hands** (set parameters)
and **eyes** (screenshot the result) in the same system, without a native GUI framework.

---

## Architecture

### The bridge — `pmetra_demo/src/wasm_bridge.rs`
Single file. Exposes `window.pmetra.{set, get, list, screenshot, load_shape, save_shape,
list_shapes, load_step, save_step, delete_shape, spawn, despawn}` as a JS API and handles
WebSocket commands from the MCP server. Uses Bevy's reflection system to discover and patch
any registered type at runtime — zero hardcoded field names.

**What it exposes (33 items):**
- CAD param components: `TowerExtension`, `SimpleCubeAtCylinder`, `ExpNurbs`, `RoundCabinSegment`
- Model switching: `CadGeneratedModelSpawner`
- Transform: `Transform`, `Visibility`, `GlobalTransform`, `Children`
- Materials: `Material:TowerExtension` etc. → `StandardMaterial` fields
- Scene globals: `GlobalAmbientLight`, `ClearColor`, `Time<Virtual>`, `DebugRenderContext`
- Engine: `DefaultOpaqueRendererMethod`, `PickingSettings`, shadow maps, audio

**Three code paths for patching:**
1. Resources → serialize → merge patch → `ReflectDeserializer` → apply
2. Components → same, but queries entities filtered to `pmetra_demo` crate
3. Materials → field-by-field via `TypedReflectDeserializer` (StandardMaterial has no
   `ReflectDeserialize`, so full round-trip fails)

**Screenshot:** `window.pmetra.screenshot()` and WS `{cmd:"screenshot"}` both call
`HtmlCanvasElement::to_data_url_with_type("image/png")` — returns base64 PNG data URL.

### MCP server — `mcp-server-rs/src/main.rs`

Single Rust binary, four roles on one port (9001):
1. **WebSocket broker** — WASM app connects to `/ws`, commands routed by sequence number
2. **HTTP API** — `GET /health`, `POST /call` (same contract as Cloudflare Worker)
3. **MCP server** — JSON-RPC over stdio (Claude Desktop / CLI connect here)
4. **Static file server** (optional, `--features embed-ui`) — serves embedded dist/ files

| | |
|---|---|
| Start | `just mcp-rs` (MCP+HTTP+WS) or `just mcp-broker` (HTTP+WS only) |
| Tools | list, get, set, screenshot, get_schema, load_shape, save_shape, list_shapes, load_step, save_step, delete_shape, simulate_touch |
| End-user binary | `just build-release` → single file, open http://localhost:9001 |

### Truck CAD loader — `pmetra_demo/src/truck_loader.rs`
Loads Truck JSON (CompressedShell or CompressedSolid) and STEP files, tessellates B-rep
geometry into Bevy meshes, and serializes back. Round-trip fidelity: Solid-format inputs
are saved back as Solids.

Key types: `TruckModel` (JSON, editable, re-tessellatable), `StepModel` (STEP, view-only,
raw data stored for re-export).

### Persistence — localStorage (single-writer)
The WASM bridge owns persistence. Shapes are stored at `pmetra_shape:{name}` and
`pmetra_step:{name}` keys in browser localStorage. On page load, `restore_persisted_shapes()`
re-queues LoadShape/LoadStep commands. `delete_shape` removes from localStorage.
The MCP server is a pass-through — it does not write to storage.

### WASM app
Built with `trunk build --release`. Auto-detects deployment mode:
- **Local dev**: trunk serve on :3000, connects to `ws://localhost:9001/ws`
- **End-user binary**: served from embedded dist/ at :9001, connects to `ws://localhost:9001/ws`
- **Cloudflare**: served from R2, connects to `wss://<host>/ws` via Durable Object

On startup the WASM connects OUT to the WebSocket endpoint. If nothing is there, it silently
continues — the JS API still works via Playwright.

### Cloudflare deployment — `workers/worker.js`
CF Worker + Durable Object (`PmetraBroker`) with WebSocket Hibernation API.
Serves static files from R2, relays MCP commands via DO. Same `/health`, `/call`, `/ws`
API as the local Rust server — tests work on both (`just cf-test`).

---

## Start sequence

```bash
# Local dev (two terminals)
just dev-all     # starts WS broker + trunk serve on :3000
                 # open http://127.0.0.1:3000?model=TowerExtension

# End-user single binary
just build-release   # builds WASM + embeds into Rust binary
./target/release/pmetra-mcp-server   # open http://localhost:9001

# Cloudflare
just cf-deploy   # builds WASM → R2 → deploys Worker + DO
just cf-test 443 pmetra-demo.gedw99.workers.dev https
```

---

## Key files

| File | Purpose |
|---|---|
| `pmetra_demo/src/wasm_bridge.rs` | The bridge — JS API, WS client, command queue, localStorage persistence |
| `pmetra_demo/src/truck_loader.rs` | Truck JSON/STEP loader, tessellation, B-rep round-trip |
| `mcp-server-rs/src/main.rs` | Rust MCP server — WS broker, HTTP API, MCP stdio, optional embedded UI |
| `workers/worker.js` | Cloudflare Worker + Durable Object (WS relay, R2 static files) |
| `wrangler.toml` | CF Worker config (R2 bucket, DO migration) |
| `IDEAS.md` | Ecosystem analysis — dimforge, Truck, inferi integration plans |
| `JUSTFILE` | All task commands |

---

## What has been proven to work

All of this was demonstrated live via Playwright MCP in prior sessions:

- **All 5 model variants** switched via `CadGeneratedModelSpawner`
- **CAD params** patched per-entity: tower height, NURBS control points, cabin window size
- **Two components mutated simultaneously** in a multi-model scene
- **Materials** changed to gold metallic — confirmed via readback
- **Transform** — model repositioned and scaled
- **Simulation time** paused/resumed
- **Physics debug wireframes** toggled
- **Lighting + background** color changed
- **Screenshot** returns valid PNG data URL from canvas
- **Truck JSON** loaded (CompressedShell and CompressedSolid formats), tessellated, rendered
- **STEP files** loaded, tessellated, rendered (view-only, raw STEP stored for re-export)
- **Shape persistence** via localStorage — shapes survive page reload, auto-restored on startup
- **Delete shape** despawns entity, removes from localStorage and SHAPE_CACHE
- **Save/load round-trip** — load cube.json → save → re-parse → same geometry

---

## Known gaps

| Gap | Fix |
|---|---|
| `PmetraGlobalSettings` not exposed | Needs `app.register_type::<PmetraGlobalSettings>()` in pmetra's plugin (their code) |
| Deferred rendering | Browser WebGPU has no GBuffer — silently falls back to Forward, not fixable |

---

## inferi integration (future)

`inferi` (dimforge) runs TinyLlama at 63 tok/s on Metal via WebGPU. Blocker: wgpu version
mismatch (inferi uses wgpu 29, Bevy 0.18 uses wgpu 27). Workaround: run on separate wgpu
device, communicate via `async_channel`.

The ONNX path (7-class model variant classifier, ~100KB) has no blockers today.
See `IDEAS.md` for the full integration plan.
