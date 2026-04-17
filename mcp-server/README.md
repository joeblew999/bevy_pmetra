# pmetra MCP server

Exposes the running Bevy/WASM Pmetra app as an [MCP](https://modelcontextprotocol.io) server so an LLM can inspect and mutate the 3D scene in real time.

## Two transport paths

| Path | How it works |
|------|-------------|
| **A — WebSocket** | The WASM app connects out to `ws://localhost:9001` on startup. Commands are sent over that socket. No browser needed for the MCP side. |
| **B — Playwright** | The server opens `http://localhost:3000` in a headless Chromium and calls `window.pmetra.set/get/list` (the JS bridge exposed by the WASM). Falls back to this if the WebSocket is not connected. |

Both paths can run simultaneously. Set/get/list all work on either path.

## MCP tools

| Tool | Description |
|------|-------------|
| `list_resources` | List all Bevy resources exposed by the WASM app |
| `get_schema` | Get the TypeScript type definition for a named resource |
| `get_resource` | Get the current JSON state of a resource |
| `set_resource` | Mutate a resource via a partial JSON patch |
| `screenshot` | Capture a PNG of the viewport (Playwright path only) |

## Prerequisites

- Bun ≥ 1.0
- WASM app running on `http://localhost:3000` (served by `trunk serve` from `pmetra_demo/`)
- Node/Bun packages: `bun install`

## Running

```sh
# Both paths (WebSocket + Playwright browser window)
bun run start

# WebSocket only (no browser window opened by the server)
bun run start:no-browser
```

The server communicates with an MCP client (e.g. Claude) over stdio.

## Testing

```sh
bun run test.ts
```

Requires the WASM app (`trunk serve`) and MCP server (`bun run start:no-browser`) to already be running. Exercises both paths and saves proof screenshots to `.playwright-mcp/`.

## Resource schemas

Schemas for known resources live in `schemas.ts`. They are generated from Rust structs via [ts-rs](https://github.com/Aleph-Alpha/ts-rs) — run `cargo test -p pmetra_demo` to regenerate `../pmetra_demo/bindings/`, then update `schemas.ts` accordingly.

## Ports

| Service | Default | Override |
|---------|---------|----------|
| WASM app | `http://localhost:3000` | `WASM_URL=...` |
| WebSocket | `ws://localhost:9001` | `WS_PORT=...` |
