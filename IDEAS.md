# TODO

## dimforge analysis


https://github.com/dimforge has some amazing stuff we can use !

All repos in the Dimforge org (fetched 2026-04-11), ordered by adoption priority for bevy_pmetra:

---

### Tier 1 — Already in use (via Cargo.lock)

No action needed. These are already available to write code against.

| Repo | Version in use | Last Push | Latest Tag | How it gets in |
| :--- | :--- | :--- | :--- | :--- |
| [nalgebra](https://github.com/dimforge/nalgebra) | 0.34.2 | 2026-03-28 | v0.34.1 | Truck CAD kernel |
| [simba](https://github.com/dimforge/simba) | 0.9.1 | 2026-02-07 | v0.9.1 | nalgebra |
| [parry](https://github.com/dimforge/parry) | 0.25.3 | 2026-04-04 | v0.26.0 | rapier3d |
| [rapier](https://github.com/dimforge/rapier) | 0.31.0 | 2026-04-02 | v017.1 | bevy_rapier3d |
| [bevy_rapier](https://github.com/dimforge/bevy_rapier) | 0.33.0 | 2026-02-25 | v0.33.0 | pmetra_demo directly |

---

### Tier 2 — Worth adding (active, stable, fills a real gap)

| Repo | Last Push | Latest Tag | Why |
| :--- | :--- | :--- | :--- |
| [glamx](https://github.com/dimforge/glamx) | 2026-04-02 | v0.2.0 | glam extensions — Bevy uses glam, Truck uses nalgebra; this helps bridge the two math worlds |

---

### Tier 3 — Watch (active but hard to use today)

These split into two distinct stacks, each with a real barrier to entry:

#### 3a — The Slang GPU stack (barrier: must learn the Slang shading language)

Dimforge is building a new GPU-first compute platform using [Slang](https://shader-slang.com/) — a Microsoft Research GPU shader language that compiles to SPIR-V, WGSL, HLSL, MSL etc. These repos are tightly coupled to each other and all require writing Slang shader code alongside Rust. Not drop-in libraries.

| Repo | Last Push | Latest Tag | What it is |
| :--- | :--- | :--- | :--- |
| [khal](https://github.com/dimforge/khal) | 2026-04-06 | — | Cross-platform GPU compute abstractions (foundation of the stack) |
| [stensor](https://github.com/dimforge/stensor) | 2026-03-11 | v0.4.1 | GPU linear algebra built on khal |
| [vortx](https://github.com/dimforge/vortx) | 2026-04-06 | — | GPU tensor library built on stensor |
| [inferi](https://github.com/dimforge/inferi) | 2026-04-06 | — | On-device GPU LLM + vision model inference (see below) |
| [nexus](https://github.com/dimforge/nexus) | 2026-01-20 | v0.2.1 | GPU physics engine (Rust + Slang), successor to rapier |
| [slosh](https://github.com/dimforge/slosh) | 2026-03-31 | v0.3.1 | GPU MPM simulation (Rust + Slang) |
| [slang-hal](https://github.com/dimforge/slang-hal) | 2026-03-04 | v0.2.0 | HAL layer for Slang GPU code |

##### inferi in detail

`inferi` actually implements four specific model types — confirmed from source:

| Model | What it does | Use in pmetra |
| :--- | :--- | :--- |
| **Llama / Qwen2** (GGUF) | Text generation — any quantised LLM from HuggingFace | Natural language → structured JSON → pmetra parameters. "Make it taller with 3 holes" → `{ height: 120, holes: 3 }` |
| **Whisper** (audio) | Speech-to-text transcription, including mic input — 30s chunks at 16kHz | Voice commands → text → LLM → pmetra parameters. Hands-free CAD control |
| **Segment Anything (SAM)** | Image segmentation — identify regions in a photo/sketch | Segment a reference image → extract shape outline → drive CAD profile geometry |
| **GPT-2** (GGUF) | Lightweight text generation | Smaller/faster alternative to Llama for constrained devices |
| **ONNX** (any `.onnx` file) | General GPU inference runtime — runs any model exported from PyTorch, TensorFlow, JAX etc. | Custom trained models: shape classifiers, style models, domain-specific networks — anything the standard model list doesn't cover |

All run **on-device GPU via wgpu** — no cloud, no API key, works in WASM/browser.

The ONNX support is the most underappreciated capability. It means inferi is not just "run Llama" — it's a general-purpose GPU inference runtime. Any model you can train in PyTorch and export to `.onnx` can run here. The test model in the repo is `mnist.onnx`, proving the basic pipeline works. Whisper itself was ported from HuggingFace's `candle` crate, showing the team is adapting rather than building from scratch.

**The concrete workflow this enables:**

```
mic input → Whisper (speech→text) → Llama (text→JSON params) → pmetra resource set
sketch photo → SAM (segment shape) → extract profile → pmetra extrude profile
```

**How hard is it to actually try?**

Harder than it looks. Specific blockers found in source:

| Blocker | Detail |
| :--- | :--- |
| `cargo-gpu` toolchain | Non-standard Rust toolchain required to compile shaders — extra install step before anything builds |
| Dioxus CLI (`dx`) | Required to run the demo app — another tool not in the standard Rust workflow |
| git-only deps | `khal` and `vortx` are not on crates.io — must point Cargo at git repos |
| Model download | GGUF models are several GB — need to source and download separately |
| SAM has a hardcoded dev path | The segment-anything code literally contains `/Users/sebcrozet/Downloads/segment-anything-huge.safetensors` — it has never been used outside Dimforge dev machines |
| No wgpu sharing docs | To embed in Bevy you'd need to share a wgpu device — there is no documentation or example for this |

**Realistic effort:**
- Getting `inferi-chat` demo running standalone: 1–2 days if lucky with toolchain setup
- Using Llama/Whisper as a library inside Bevy: significant, undocumented work
- SAM: not usable outside Dimforge right now

**Verdict — fully tried (2026-04-11), cloned to `/Users/apple/workspace/go/src/github.com/dimforge/inferi`:**

| Step | Result |
| :--- | :--- |
| `cargo install --git https://github.com/rust-gpu/rust-gpu cargo-gpu` | Works — NOTE: repo moved from `Rust-GPU/cargo-gpu` (now archived) |
| `cargo gpu install --auto-install-rust-toolchain` | Works — installs nightly-2025-10-28 + compiles `rustc_codegen_spirv`, ~2 min |
| `cargo build -p inferi-chat --release --features desktop` | Works — ~2 min |
| CPU + Qwen2-0.5B | **PANICS** — index out of bounds in khal-std |
| CPU + TinyLlama (debug build) | **TOO SLOW** — never generates first token after 10 min |
| CPU + TinyLlama (release build) | **TOO SLOW** — never generates first token after 2 min |
| **WebGPU + TinyLlama (Metal on Mac)** | **WORKS — 63 tok/s**, response in ~2s |
| **WebGPU + TinyLlama (multi-turn chat)** | **WORKS — 50→25 tok/s** as context grows; stdin loops when closed |
| WebGPU + Qwen2-0.5B | **SILENT FAILURE** — exits 0, generates 0 tokens (tokenizer bug) |
| `cargo run -p inferi --release --features onnx --example run_onnx` | **WORKS** — mnist digit classifier correct [1,10] output |
| `cargo build -p inferi-whisper-chat --release --features desktop` | **WORKS** — binary built, whisper-tiny model downloaded |

The CPU backend is effectively broken for inference. WebGPU (Metal on Mac) works well.

A `mise.toml` is in the inferi folder covering setup, model download, build, and run. `mise run setup && mise run models && mise run build && mise run run` is the full sequence.

**Root cause of Qwen2 silent failure (found in source):**

Qwen2 uses a BPE tokenizer (tiktoken-based). The GGUF metadata for Qwen2 has `tokenizer.ggml.model = "gpt2"`. inferi's `Gpt2Tokenizer` handles that string but implements a GPT-2-style vocabulary decode that is incompatible with Qwen2's 151k-token vocabulary. The tokenizer encodes the prompt but the resulting token IDs are garbage — the model generates tokens that immediately hit EOS or loop without printing.

**Tokenizer support matrix (confirmed from source):**

| Model family | `tokenizer.ggml.model` | inferi tokenizer | Works? |
| :--- | :--- | :--- | :--- |
| TinyLlama, Llama 1/2, Mistral 7B | `"llama"` (SentencePiece BPE) | `LlamaTokenizer` | ✓ |
| Qwen2, Llama 3.x, GPT-2 | `"gpt2"` (BPE, different vocab) | `Gpt2Tokenizer` | ✗ broken |

**Practical consequence:** Only Llama-1/2-family models work today. TinyLlama is the smallest (1.1B). Next size up would be Mistral-7B (3.8GB Q4_K_M) — no working 2–4B option in this family.

**API for embedding in Bevy (confirmed from source):**

The inferi library API is clean and usable:

```rust
// 1. Create GPU backend (creates its own wgpu device)
let webgpu = WebGpu::new(features, limits).await?;
let backend = GpuBackend::WebGpu(webgpu);

// 2. Load model
let mmap = memmap2::Mmap::map(&file)?;
let gguf = Gguf::from_bytes(&mmap[..])?;
let llm = ChatLlm::from_gguf(&backend, &gguf).await?;

// 3. Run inference with streaming callback
llm.forward(&backend, prompt, sampler, template, next_pos, |event| {
    if let ChatEvent::Token { string, next_pos, .. } = event {
        // stream token to UI
    }
    Ok(())
}).await?;
```

**Critical blocker for Bevy integration:**

| Issue | Detail |
| :--- | :--- |
| **wgpu version mismatch** | inferi uses wgpu **29.0.1**, Bevy 0.18 uses wgpu **27.0.1** — different majors, incompatible types |
| **No public device sharing** | `WebGpu` stores `device: Device` and `queue: Queue` as private fields; no constructor from existing device; can't pass Bevy's render device in |
| **No `WebGpu::from_device()`** | To share a device you'd need to fork khal and add a `from_device(device, queue)` constructor |

**Viable embedding strategy today:**

Run inferi on a **separate wgpu device** (same physical GPU, different logical context). Modern GPU drivers schedule multiple contexts without issue. The cost is VRAM — both Bevy's renderer and the LLM model compete for GPU memory. TinyLlama Q4_K_M is ~669MB; Mistral-7B Q4_K_M is ~3.8GB.

```
┌─────────────────────┐    channel    ┌─────────────────────┐
│   Bevy main thread  │ ←──────────── │  inferi async task  │
│   (render, physics) │               │  (own wgpu device)  │
│   wgpu 27           │               │  wgpu 29            │
└─────────────────────┘               └─────────────────────┘
         ↑ pmetra params                      ↑ GGUF model
         └──── via Bevy Events/Resources ─────┘
```

The correct integration is:
1. Spawn inferi in a `bevy_tasks::IoTaskPool` task at startup
2. Load the model once, keep it alive
3. Send prompts via a `crossbeam-channel` or `async_channel` 
4. Receive token stream back; update a Bevy `Resource` with the JSON output
5. Parse JSON, set pmetra parameters

**What TinyLlama 1.1B actually can and can't do (tested via `channel_llm` example):**

- ✓ Simple Q&A in 2s at 52–63 tok/s
- ✓ Multi-turn conversation (context grows, slows to 25 tok/s at ~10 turns)
- ✓ **CAD model name classification from natural language** — tested and works:
  - "make me a tower extension" → correctly selects `TowerExtension`
  - "I want something round like a cabin" → correctly selects `RoundCabinSegment`
- ✗ Output-ONLY-JSON instruction following — wraps correct answer in prose and markdown fences, but the JSON itself (`{"model": "TowerExtension"}`) is always present and extractable by regex
- ✗ Numerical parameter extraction format — given "80mm tall, radius 12mm, 4 holes of radius 2mm" it writes Python code instead of JSON, BUT the Python code contains correct values (height=80, radius=12, holes=4, hole_radius=2) — the model understands, just can't format output correctly
- ✗ Few-shot schema generalization — given an example `{"height": 80, "radius": 12}` and asked to extract from a new sentence with a different schema, it copies the original values instead of extracting new ones. Can't generalize across schemas at 1.1B params.

**Practical extraction strategy that works today:**

```rust
// Response contains {"model": "RoundCabinSegment"} somewhere in prose.
// Extract with:
let re = Regex::new(r#"\{"model":\s*"([^"]+)"\}"#).unwrap();
if let Some(cap) = re.captures(&response) {
    let model_name = &cap[1]; // "RoundCabinSegment"
}
```

This means TinyLlama 1.1B is **already useful for model variant selection** in pmetra today, running at ~53 tok/s on Metal.

**The `channel_llm` example (added to inferi-chat/examples/):**

Shows the Bevy-style integration pattern:
- LLM worker runs in a background `async_std::task` (maps to `IoTaskPool::get().spawn()` in Bevy)
- Prompt sent via `async_channel::Sender<LlmCmd>`
- Tokens streamed back via `async_channel::Receiver<LlmEvent>`
- Main thread (or Bevy system) polls the receiver
- Model loaded once at startup, reused for all queries

Run with:
```
cargo run -p inferi-chat --release --features desktop --example channel_llm \
  -- models/tinyllama-1.1b-chat-q4_k_m.gguf "make me a tower extension"
```

For reliable JSON output from natural language you need ≥7B (Mistral-7B-Instruct) or a fine-tuned smaller model. The ONNX path could work for simpler classification tasks.

#### 3b — The WebGPU compute stack (barrier: WebGPU compute is still maturing)

These use wgpu (the same backend Bevy uses) for GPU compute, so they are more compatible — but WebGPU compute support is still inconsistent across platforms.

| Repo | Last Push | Latest Tag | What it is |
| :--- | :--- | :--- | :--- |
| [wgsparkl](https://github.com/dimforge/wgsparkl) | 2025-05-21 | — | MPM physics simulation via WebGPU |
| [bevy_wgsparkl](https://github.com/dimforge/bevy_wgsparkl) | 2025-04-04 | — | Bevy plugin for wgsparkl |

#### 3c — Usable today if targeting web

| Repo | Last Push | Latest Tag | What it is |
| :--- | :--- | :--- | :--- |
| [rapier.js](https://github.com/dimforge/rapier.js) | 2025-11-05 | v0.19.3 | Rapier compiled to WASM with JS bindings — stable and well maintained |

---

### Tier 4 — Skip (inactive, legacy, or out of scope)

| Repo | Last Push | Latest Tag | Reason |
| :--- | :--- | :--- | :--- |
| [salva](https://github.com/dimforge/salva) | 2025-02-02 | v0.7.0 | Fluid sim — not relevant |
| [bevy_salva](https://github.com/dimforge/bevy_salva) | 2020-12-08 | — | Dead |
| [sparkl](https://github.com/dimforge/sparkl) | 2024-07-29 | v0.2.1 | Superseded by wgsparkl/slosh |
| [Steadyum](https://github.com/dimforge/Steadyum) | 2024-08-09 | — | Physics sandbox app, stalled |
| [kiss3d](https://github.com/dimforge/kiss3d) | 2026-04-05 | v0.41.0 | Competitor to Bevy, not a plugin |
| [slai](https://github.com/dimforge/slai) | 2025-09-20 | — | GPU AI inference, too early |
| [ncollide](https://github.com/dimforge/ncollide) | 2023-01-31 | v0.33.0 | Legacy, replaced by parry |
| [nphysics](https://github.com/dimforge/nphysics) | 2021-07-27 | v0.20.0 | Legacy, replaced by rapier |
| [alga](https://github.com/dimforge/alga) | 2023-02-05 | v0.9.3 | Legacy, replaced by simba |

---

## Truck ecosystem (ricosjp)

The CAD kernel at the core of bevy_pmetra. Worth knowing what else exists around it.

| Repo | Last Push | Latest Tag | What it is |
| :--- | :--- | :--- | :--- |
| [ricosjp/truck](https://github.com/ricosjp/truck) | 2026-04-04 | truck-topology-v0.6.0 | The CAD kernel — B-rep solids, NURBS, mesh generation |
| [joeblew999/truck](https://github.com/joeblew999/truck) | 2026-04-05 | — | **Your own fork** — actively ahead of upstream |
| [ricosjp/ruststep](https://github.com/ricosjp/ruststep) | 2025-03-19 | ruststep-v0.4.0 | STEP file import/export for Truck — key for CAD interop |
| [ricosjp/femio](https://github.com/ricosjp/femio) | 2024-05-24 | — | FEM mesh I/O — relevant if doing structural simulation on CAD parts |

**Notable active forks of truck:**
- `kineticflex/truck` — 2026-04-04
- `rstkit/truck` — 2026-03-05
- `tucanos/truck` — 2025-10-09

---

## Alternative CAD kernels

Other Rust-native approaches to solid modeling. Useful as reference or complement.

| Repo | Last Push | Latest Tag | What it is | Barrier |
| :--- | :--- | :--- | :--- | :--- |
| [hannobraun/fornjot](https://github.com/hannobraun/fornjot) | 2026-04-07 | v0.49.0 | B-Rep CAD kernel, code-first, experimental | Pre-production, API unstable |
| [mkeeter/fidget](https://github.com/mkeeter/fidget) | 2026-04-06 | v0.4.2 | Implicit surface eval + mesh gen via JIT compiler | Different paradigm to B-rep — complements rather than replaces Truck |
| [bschwind/opencascade-rs](https://github.com/bschwind/opencascade-rs) | 2026-03-28 | — | Rust bindings to OpenCascade (OCCT) — fillets, chamfers, STEP/STL/DXF | FFI to C++, heavy dependency, not WASM-friendly |
| [ecto/vcad](https://github.com/ecto/vcad) | 2026-04-01 | — | Parametric CAD with CSG, compiles to WASM, exports STEP/STL/GLB/DXF | Early stage, no tags |
| [KittyCAD/modeling-app](https://github.com/KittyCAD/modeling-app) | 2026-04-11 | v1.2.1 | Zoo Design Studio — GPU-accelerated, AI-assisted, cloud CAD | Cloud-dependent, different target market |

**Fidget** is the most interesting complement — implicit surfaces (think: distance fields, generative shapes) are a different modeling paradigm to Truck's B-rep but the two can coexist: Truck generates precise CAD geometry, Fidget generates organic/procedural forms.

---

## Rust geometry & mesh libraries

Standalone crates for geometry math, mesh processing, and format I/O.

### Mesh processing

| Crate | Last Push | Latest Tag | What it is |
| :--- | :--- | :--- | :--- |
| [meshopt](https://github.com/gwihlidal/meshopt-rs) | 2025-10-09 | v0.6.2 | GPU mesh optimization, LOD simplification, vertex cache optimization |
| [i_overlay](https://github.com/iShape-Rust/i_overlay) | 2026-03-23 | v4.5.1 | Boolean polygon/mesh operations (union, subtract, intersect) |
| [stl_io](https://crates.io/crates/stl_io) | 2026-03-15 | v0.11.0 | STL file read/write |
| [tobj](https://crates.io/crates/tobj) | 2025-01-20 | v4.0.3 | OBJ/MTL file loading |

### 2D geometry (useful for profile-based CAD — extrude, revolve)

| Crate | Last Push | Latest Tag | What it is |
| :--- | :--- | :--- | :--- |
| [lyon](https://github.com/nical/lyon) | 2026-03-08 | — | 2D path tessellation — bezier curves, arcs, fills, strokes → triangles |
| [kurbo](https://github.com/linebender/kurbo) | 2025-11-27 | v0.13.0 | 2D curves and paths, used by Linebender/Vello |
| [geo](https://github.com/georust/geo) | 2025-12-05 | v0.32.0 | 2D geospatial geometry — polygons, booleans, simplification |
| [spade](https://github.com/Stoeoef/spade) | 2026-03-24 | v2.15.1 | Delaunay triangulation and spatial indexing |

### CSG / solid boolean ops

| Crate | Last Push | What it is |
| :--- | :--- | :--- |
| [csgrs](https://github.com/timschmidt/csgrs) | Active | CSG (union/subtract/intersect) in Rust; integrates with nalgebra/parry |

---

## Bevy ecosystem plugins

Plugins that complement bevy_pmetra's generated meshes within the Bevy app.

### Procedural mesh generation

| Repo | Last Push | What it is |
| :--- | :--- | :--- |
| [bevy-procedural/meshes](https://github.com/bevy-procedural/meshes) | 2026-01-24 | Procedural mesh builder; uses Lyon for 2D, extrudes to 3D, optimizes with meshopt |
| [bevy-procedural/modelling](https://github.com/bevy-procedural/modelling) | 2026-01-24 | Framework-agnostic boolean ops, subdivisions, curved surfaces — early stage |
| [bevy_copperfield](https://github.com/Hexorg/bevy_copperfield) | Active | Blender geometry nodes–style modelling; half-edge mesh; extrude/subdivide/bevel |

### Mesh tools & rendering

| Plugin | What it is |
| :--- | :--- |
| `bevy_mod_raycast` | Raycasting against meshes — picking, hit-testing on CAD parts |
| `bevy_mod_mesh_tools` | Mesh manipulation and transformation helpers |
| `bevy_mesh_outline` | 3D mesh outlines — useful for CAD-style rendering |
| `bevy_grid_mesh` | Procedural terrain via height maps |
| `bevy-earcutr` | Polygon triangulation → Bevy mesh |

### Key gap: no native CSG in Bevy

Bevy issue [#13790](https://github.com/bevyengine/bevy/issues/13790) for boolean mesh ops is marked "not planned". This means CSG on pmetra-generated meshes requires a separate crate (`csgrs`, `i_overlay`, or `bevy-procedural/modelling`).

---

## WASM / web delivery

bevy_pmetra already runs on WASM (see the demo). Key considerations:

| Topic | Notes |
| :--- | :--- |
| **Trunk** | Already configured (`Trunk.toml` present) — handles WASM bundling |
| **rapier.js** | If you need physics on the JS side (not Rust), `rapier.js` v0.19.3 is stable |
| **opencascade-rs** | Not WASM-friendly — FFI to C++, avoid for web targets |
| **fidget** | WASM-compatible — JIT compiles to native or WASM |
| **vcad** | WASM-first design — worth watching for export format ideas |
| **WebGPU compute** | `wgsparkl`/`bevy_wgsparkl` only work where WebGPU compute is available (Chrome, not Safari/Firefox yet) |

---

## Priorities — what to actually do

Synthesised from all of the above. Ordered by value vs effort.

### Do now (low effort, high value)

| Action | Why |
| :--- | :--- |
| Add `glamx` | Bridges glam ↔ nalgebra — already have both in the tree, reduces manual conversion boilerplate |
| Add `ruststep` | STEP import/export via `ricosjp/ruststep` — enables real CAD file interop, actively maintained |
| Add `meshopt` | LOD + GPU mesh optimisation on pmetra-generated meshes — single crate, well maintained |
| Add `stl_io` | STL export alongside the existing GLTF — common format for 3D printing / CAD handoff |

### Evaluate next (medium effort, clear value)

| Action | Why |
| :--- | :--- |
| Evaluate `fidget` | Implicit surfaces complement Truck's B-rep — organic/generative shapes Truck can't do |
| Evaluate `i_overlay` | Boolean mesh ops (union/subtract/intersect) — fills the CSG gap Bevy won't fix |
| Evaluate `bevy_copperfield` | Half-edge mesh + Blender-style ops on pmetra output — subdivide, bevel, extrude |
| Study `vcad` | WASM-first parametric CAD with CSG — good reference for design patterns even if not adopted directly |

### Watch (high potential, not ready)

| Action | Why |
| :--- | :--- |
| Watch Dimforge Slang stack (`nexus`, `khal`, `vortx`) | GPU-accelerated geometry processing — relevant once stable |
| Watch `fornjot` | Pure Rust B-Rep kernel — could eventually replace or complement Truck |
| Watch `KittyCAD` | Most active CAD org right now — design patterns and format support worth tracking |

### Don't touch

`opencascade-rs` (not WASM-friendly), anything fluid/MPM, all legacy Dimforge repos.

---

## inferi integration path (concrete steps)

When the time comes to embed LLM inference in pmetra, the sequence is:

1. **Wait for wgpu alignment** — inferi uses wgpu 29, Bevy 0.18 uses wgpu 27. Either inferi needs to downgrade or Bevy needs to upgrade. Check `bevy/Cargo.toml` wgpu version before starting.

2. **Or fork khal** — add `WebGpu::from_device(device: wgpu::Device, queue: wgpu::Queue) -> Self` to share Bevy's existing GPU device. This avoids the version issue if you pin them together.

3. **Wire up the task pool bridge:**
   ```rust
   // In a Bevy plugin
   fn setup_inferi(mut commands: Commands) {
       let (prompt_tx, prompt_rx) = async_channel::unbounded::<String>();
       let (token_tx, token_rx) = async_channel::unbounded::<String>();
       commands.insert_resource(InferiChannels { prompt_tx, token_rx });
       IoTaskPool::get().spawn(async move {
           let backend = init_webgpu_backend().await.unwrap();
           let gguf = load_gguf("models/mistral-7b-instruct-q4_k_m.gguf");
           let llm = ChatLlm::from_gguf(&backend, &gguf).await.unwrap();
           while let Ok(prompt) = prompt_rx.recv().await {
               llm.forward(&backend, prompt.into(), ..., |ev| {
                   if let ChatEvent::Token { string, .. } = ev {
                       token_tx.send_blocking(string).ok();
                   }
                   Ok(())
               }).await.ok();
           }
       }).detach();
   }
   ```

4. **Model choice for structured output:** TinyLlama 1.1B is too small. Minimum for reliable JSON: Mistral-7B-Instruct-v0.3 Q4_K_M (~3.8GB). Uses Llama tokenizer — works in inferi.

5. **Alternatively, ONNX for classification tasks:** If you only need to classify which pmetra model variant to use (7 choices), train a tiny classifier in PyTorch, export to `.onnx`, run via inferi ONNX runtime. This works today with no model size concerns.
