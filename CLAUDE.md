# bevy_pmetra — AI Session Context

## Objective

Build a system where an AI (Claude) can fully control a parametric CAD application running
in a browser, with no human in the loop. The goal is AI-driven CAD: describe what you want,
the geometry changes.

```
user / AI intent
      ↓
MCP server (Rust or TypeScript)
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

### MCP servers — two implementations, same protocol

| | TypeScript (`mcp-server/`) | Rust (`mcp-server-rs/`) |
|---|---|---|
| Start | `just mcp-ts` | `just mcp-rs` |
| Tools | list, get, set, screenshot, load_shape, save_shape, list_shapes, load_step, save_step, delete_shape | same |
| WS broker | hosts port 9001 | hosts port 9001 |
| MCP transport | stdio | stdio |
| Extra | Playwright path (opens browser itself), `get_schema` tool | no extra deps |

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

Only run one at a time — both host port 9001.

### WASM app
Built with `trunk build --release`, served with `just serve-pmetra-demo-web-release`.
On startup the WASM connects OUT to `ws://localhost:9001`. If nothing is there, it silently
continues — the JS API still works via Playwright.

---

## Start sequence

```bash
# Terminal 1 — build and serve the WASM app
just build-pmetra-demo-web
just serve-pmetra-demo-web-release   # http://127.0.0.1:3000

# Terminal 2 — MCP server (pick one)
just mcp-rs    # Rust server (recommended — no Node.js)
just mcp-ts    # TypeScript server (has Playwright path too)
```

The WASM app connects to the MCP server on startup. Reload the browser after starting
the MCP server if it wasn't running when the page loaded.

---

## Key files

| File | Purpose |
|---|---|
| `pmetra_demo/src/wasm_bridge.rs` | The bridge — JS API, WS client, command queue, localStorage persistence |
| `pmetra_demo/src/truck_loader.rs` | Truck JSON/STEP loader, tessellation, B-rep round-trip |
| `mcp-server-rs/src/main.rs` | Rust MCP server (recommended) |
| `mcp-server/index.ts` | TypeScript MCP server (has Playwright path) |
| `mcp-server/schemas.ts` | TypeScript schemas for all 33 exposed items |
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
