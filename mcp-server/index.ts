/**
 * Pmetra MCP Server
 *
 * Two independent transport paths to the Bevy/WASM app:
 *
 *   PATH A — WebSocket (no browser needed):
 *     This server listens on ws://localhost:9001
 *     The WASM app connects out to it on startup
 *     Commands: set_resource, get_resource, list_resources
 *
 *   PATH B — Playwright (browser automation):
 *     Opens http://localhost:3000 in a browser
 *     Uses window.pmetra.set/get/list JS API
 *     Commands: same + screenshot
 *
 * Either path works standalone. Both can run simultaneously.
 *
 * Usage:
 *   bun run index.ts              # both paths
 *   NO_PLAYWRIGHT=1 bun run index.ts   # WebSocket only
 *   NO_WEBSOCKET=1 bun run index.ts   # Playwright only
 */

import { WebSocketServer, WebSocket } from "ws";
import { chromium, type Browser, type Page } from "playwright";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { getSchema, knownResources, RESOURCE_SCHEMAS } from "./schemas.js";

const WASM_URL = process.env.WASM_URL ?? "http://localhost:3000";
const WS_PORT = parseInt(process.env.WS_PORT ?? "9001");
const USE_PLAYWRIGHT = !process.env.NO_PLAYWRIGHT;
const USE_WEBSOCKET = !process.env.NO_WEBSOCKET;

// ---------------------------------------------------------------------------
// PATH A — WebSocket server
// ---------------------------------------------------------------------------

let wasmSocket: WebSocket | null = null; // connected WASM client
const pendingReplies = new Map<string, (v: unknown) => void>();
let replySeq = 0;

function startWebSocketServer() {
  const wss = new WebSocketServer({ port: WS_PORT });

  wss.on("listening", () => {
    console.error(`[ws] listening on ws://localhost:${WS_PORT}`);
  });

  wss.on("error", (e: Error) => {
    console.error(`[ws] server error (port ${WS_PORT} may be busy):`, e.message);
  });

  wss.on("connection", (ws) => {
    console.error("[ws] WASM app connected");
    wasmSocket = ws;

    ws.on("message", (raw) => {
      try {
        const msg = JSON.parse(raw.toString());
        // Route reply to waiting promise
        if (msg._seq !== undefined) {
          const resolve = pendingReplies.get(String(msg._seq));
          if (resolve) {
            pendingReplies.delete(String(msg._seq));
            resolve(msg);
          }
        }
      } catch {
        console.error("[ws] bad message", raw);
      }
    });

    ws.on("close", () => {
      console.error("[ws] WASM app disconnected");
      wasmSocket = null;
    });
  });
}

/** Send a command to WASM via WebSocket and wait for reply. */
function wsSend(cmd: object): Promise<unknown> {
  return new Promise((resolve, reject) => {
    if (!wasmSocket || wasmSocket.readyState !== WebSocket.OPEN) {
      reject(new Error("WASM app not connected via WebSocket"));
      return;
    }
    const seq = replySeq++;
    pendingReplies.set(String(seq), resolve);
    setTimeout(() => {
      if (pendingReplies.has(String(seq))) {
        pendingReplies.delete(String(seq));
        reject(new Error("WebSocket reply timeout"));
      }
    }, 5000);
    wasmSocket.send(JSON.stringify({ ...cmd, _seq: seq }));
  });
}

// ---------------------------------------------------------------------------
// PATH B — Playwright
// ---------------------------------------------------------------------------

let browser: Browser | null = null;
let page: Page | null = null;

async function startPlaywright() {
  browser = await chromium.launch({ headless: false });
  page = await browser.newPage();
  await page.goto(WASM_URL);
  // Wait for WASM to mount window.pmetra
  await page.waitForFunction(() => typeof (window as any).pmetra !== "undefined", {
    timeout: 30000,
  });
  console.error("[playwright] window.pmetra ready");
}

async function playwrightSet(resource: string, value: object): Promise<unknown> {
  if (!page) throw new Error("Playwright not running");
  return page.evaluate(
    ([r, v]) => (window as any).pmetra.set(r, JSON.stringify(v)),
    [resource, value] as [string, object]
  );
}

async function playwrightGet(resource: string): Promise<unknown> {
  if (!page) throw new Error("Playwright not running");
  const raw = await page.evaluate(
    (r) => (window as any).pmetra.get(r),
    resource
  );
  return raw ? JSON.parse(raw as string) : null;
}

async function playwrightList(): Promise<string[]> {
  if (!page) throw new Error("Playwright not running");
  const raw = await page.evaluate(() => (window as any).pmetra.list());
  return raw ? JSON.parse(raw as string) : [];
}

async function playwrightScreenshot(): Promise<string> {
  if (!page) throw new Error("Playwright not running");
  const buf = await page.screenshot({ type: "png" });
  return buf.toString("base64");
}

// ---------------------------------------------------------------------------
// Generic helpers — try WebSocket first, fall back to Playwright
// ---------------------------------------------------------------------------

async function doSet(resource: string, value: object) {
  if (USE_WEBSOCKET && wasmSocket?.readyState === WebSocket.OPEN) {
    return wsSend({ cmd: "set", resource, value });
  }
  if (USE_PLAYWRIGHT && page) {
    return playwrightSet(resource, value);
  }
  throw new Error("No transport available — start WASM app or Playwright");
}

async function doGet(resource: string) {
  if (USE_WEBSOCKET && wasmSocket?.readyState === WebSocket.OPEN) {
    return wsSend({ cmd: "get", resource });
  }
  if (USE_PLAYWRIGHT && page) {
    return playwrightGet(resource);
  }
  throw new Error("No transport available");
}

async function doList() {
  if (USE_WEBSOCKET && wasmSocket?.readyState === WebSocket.OPEN) {
    return wsSend({ cmd: "list" });
  }
  if (USE_PLAYWRIGHT && page) {
    return playwrightList();
  }
  throw new Error("No transport available");
}

// ---------------------------------------------------------------------------
// MCP tools
// ---------------------------------------------------------------------------

const server = new McpServer({
  name: "pmetra",
  version: "1.0.0",
});

const knownSchemaList = knownResources().map((name) => {
  const s = RESOURCE_SCHEMAS[name]!;
  return `  ${name}: ${s.tsType}  // ${s.description}`;
}).join("\n");

server.tool(
  "list_resources",
  "List all Bevy resources exposed by the running Pmetra WASM app. " +
    "Resources with known schemas: " + knownResources().join(", "),
  {},
  async () => {
    const result = await doList();
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "get_schema",
  "Get the TypeScript type definition and description for a named Bevy resource. " +
    "Call this before set_resource to know valid field names and values. " +
    `Known resources: ${knownResources().join(", ")}`,
  { resource: z.string().describe("Short resource name, e.g. CadGeneratedModelSpawner") },
  async ({ resource }) => {
    const schema = getSchema(resource);
    if (!schema) {
      return { content: [{ type: "text", text: `No schema registered for '${resource}'. Use list_resources to see all available resources, then get_resource to inspect its live JSON.` }] };
    }
    return {
      content: [{
        type: "text",
        text: `// ${schema.description}\ntype ${resource} = ${schema.tsType}`,
      }],
    };
  }
);

server.tool(
  "get_resource",
  "Get the current JSON state of a Bevy resource by short type name. " +
    "Use get_schema first to understand the type structure.",
  { resource: z.string().describe("Short type name, e.g. CadGeneratedModelSpawner") },
  async ({ resource }) => {
    const result = await doGet(resource);
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "set_resource",
  "Mutate a Bevy resource by applying a JSON patch (partial update — unspecified fields keep their current value).\n\n" +
    "WORKFLOW: get_schema → (optionally get_resource) → set_resource\n\n" +
    "Known resource schemas:\n" + knownSchemaList,
  {
    resource: z.string().describe("Short type name, e.g. CadGeneratedModelSpawner"),
    value: z.record(z.unknown()).describe(
      'Partial JSON patch. Examples:\n' +
      '  {"selected_params":"ExpNurbsSolid"}\n' +
      '  {"brightness":800}\n' +
      '  {"LinearRgba":{"red":0.1,"green":0.2,"blue":0.3,"alpha":1.0}}\n' +
      '  {"color":{"LinearRgba":{"red":1,"green":0,"blue":0,"alpha":1}},"brightness":600}'
    ),
  },
  async ({ resource, value }) => {
    const result = await doSet(resource, value as object);
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "screenshot",
  "Take a screenshot of the Pmetra app. Requires Playwright path.",
  {},
  async () => {
    if (!USE_PLAYWRIGHT || !page) {
      return { content: [{ type: "text", text: "Playwright not running. Start without NO_PLAYWRIGHT." }] };
    }
    const base64 = await playwrightScreenshot();
    return { content: [{ type: "image", data: base64, mimeType: "image/png" }] };
  }
);

// ── Truck shape/STEP file tools ───────────────────────────────────────────────

server.tool(
  "load_shape",
  "Load a Truck JSON shape file into the scene as a rendered mesh. " +
    "Pass the full JSON content as 'data'. Optionally provide a transform.",
  {
    name: z.string().describe("Display name for the loaded shape, e.g. 'cube'"),
    data: z.string().describe("Full Truck JSON content (CompressedShell format)"),
    transform: z.object({
      translation: z.tuple([z.number(), z.number(), z.number()]).optional(),
    }).optional().describe("Optional position: {translation: [x, y, z]}"),
  },
  async ({ name, data, transform }) => {
    const cmd: Record<string, unknown> = { cmd: "load_shape", name, data };
    if (transform) cmd.transform = transform;
    const result = await wsSend(cmd);
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "save_shape",
  "Save a loaded TruckModel entity back to Truck JSON format. " +
    "Returns the full JSON string of the shell geometry.",
  {
    name: z.string().describe("Name of the TruckModel to save (as used in load_shape)"),
  },
  async ({ name }) => {
    const result = await wsSend({ cmd: "save_shape", name });
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "list_shapes",
  "List all loaded Truck shapes (both JSON and STEP). " +
    "Returns name and format for each loaded model.",
  {},
  async () => {
    const result = await wsSend({ cmd: "list_shapes" });
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "load_step",
  "Load a STEP file into the scene as rendered mesh(es). " +
    "Pass the full STEP file content as 'data'. STEP models are view-only " +
    "(the raw STEP data is stored for re-export via save_step).",
  {
    name: z.string().describe("Display name for the loaded model, e.g. 'bracket'"),
    data: z.string().describe("Full STEP file content (ISO-10303-21 format)"),
    transform: z.object({
      translation: z.tuple([z.number(), z.number(), z.number()]).optional(),
    }).optional().describe("Optional position: {translation: [x, y, z]}"),
  },
  async ({ name, data, transform }) => {
    const cmd: Record<string, unknown> = { cmd: "load_step", name, data };
    if (transform) cmd.transform = transform;
    const result = await wsSend(cmd);
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "save_step",
  "Save a loaded StepModel entity's raw STEP data. " +
    "Returns the original STEP file content.",
  {
    name: z.string().describe("Name of the StepModel to save (as used in load_step)"),
  },
  async ({ name }) => {
    const result = await wsSend({ cmd: "save_step", name });
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

server.tool(
  "delete_shape",
  "Delete a loaded shape — despawns entity from the scene and removes " +
    "from browser localStorage. Use list_shapes to see what's loaded.",
  {
    name: z.string().describe("Name of the shape to delete (as used in load_shape/load_step)"),
  },
  async ({ name }) => {
    const result = await wsSend({ cmd: "delete_shape", name });
    return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
  }
);

// ---------------------------------------------------------------------------
// Start everything
// ---------------------------------------------------------------------------

if (USE_WEBSOCKET) startWebSocketServer();

if (USE_PLAYWRIGHT) {
  startPlaywright().catch((e) => {
    console.error("[playwright] failed to start:", e.message);
    console.error("[playwright] running in WebSocket-only mode");
  });
}

const transport = new StdioServerTransport();
await server.connect(transport);
console.error("[mcp] pmetra MCP server ready");
