/**
 * Cloudflare Worker — serves pmetra WASM app from R2 + MCP relay via Durable Object.
 *
 * Routes:
 *   GET  /            → index.html from R2 (SPA fallback for unknown paths)
 *   GET  /ws          → WebSocket upgrade → Durable Object (WASM app connects here)
 *   GET  /health      → {"ok":true,"wasm_connected":bool}
 *   POST /call        → forward JSON command to WASM app via DO, return reply
 *   GET  /<file>      → static file from R2
 *
 * The WASM app auto-connects to wss://<host>/ws on page load (detected by protocol).
 * AI (Claude) or curl hits /call — same HTTP API as the local Rust MCP server.
 *
 * Durable Object uses WebSocket Hibernation — zero cost when idle, wakes on message.
 */

import manifest from "../pmetra-manifest.json";

const MIME_TYPES = {
  ".html": "text/html",
  ".js": "application/javascript",
  ".wasm": "application/wasm",
  ".css": "text/css",
  ".json": "application/json",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".svg": "image/svg+xml",
  ".txt": "text/plain",
};

// ── Agent readiness — all metadata derived from pmetra-manifest.json ───────

const LINK_HEADER =
  '</.well-known/api-catalog>; rel="api-catalog", </.well-known/mcp/server-card.json>; rel="service-desc"';

// Helper: model name (works whether models are strings or objects).
const modelName = (m) => (typeof m === "string" ? m : m.name);

// SKILL.md content (origin-independent so digest is stable).
const SKILL_MD = [
  `# ${manifest.skills[0].name}`,
  "",
  manifest.skills[0].description,
  "",
  "## Workflow",
  "",
  "1. **Discover** — `list_resources` to see everything you can control",
  "2. **Inspect** — `get_schema` to learn field names and types",
  "3. **Read** — `get_resource` to see current values",
  "4. **Modify** — `set_resource` to change parameters (triggers geometry rebuild)",
  "5. **Verify** — `screenshot` to see the result",
  "",
  "## Quick Start",
  "",
  "```json",
  '{"cmd": "list"}',
  '{"cmd": "schema", "name": "TowerExtension"}',
  '{"cmd": "get", "resource": "TowerExtension"}',
  '{"cmd": "set", "resource": "TowerExtension", "value": {"tower_length": 3.0}}',
  '{"cmd": "screenshot"}',
  "```",
  "",
  "## Available Models",
  "",
  ...manifest.models.flatMap((m) => {
    const lines = [`### ${modelName(m)}`];
    if (m.description) lines.push("", m.description);
    if (m.parameters && m.parameters.length > 0) {
      lines.push("", "| Parameter | Type | Default | Description |");
      lines.push("| --- | --- | --- | --- |");
      m.parameters.forEach((p) => {
        const def = p.default !== undefined ? String(p.default) : "—";
        lines.push(`| \`${p.name}\` | ${p.type} | ${def} | ${p.description} |`);
      });
    }
    lines.push("");
    return lines;
  }),
  "## MCP Tools",
  "",
  ...manifest.tools.map((t) => `- **${t.name}** — ${t.description}`),
  "",
  "## Recipes",
  "",
  "**Change materials to gold metallic:**",
  "```json",
  '{"cmd": "set", "resource": "Material:TowerExtension",',
  ' "value": {"base_color":{"Srgba":{"red":1.0,"green":0.84,"blue":0.0,"alpha":1.0}},"metallic":1.0}}',
  "```",
  "",
  "**Switch active model:**",
  "```json",
  '{"cmd": "set", "resource": "CadGeneratedModelSpawner",',
  ` "value": {"selected_params": "${modelName(manifest.models[0])}"}}`,
  "```",
  "",
  "**Reposition geometry:**",
  "```json",
  '{"cmd": "set", "resource": "Transform", "value": {"translation": [1.0, 0.0, 0.0], "scale": [2.0, 2.0, 2.0]}}',
  "```",
  "",
  "**Pause simulation time:**",
  '```json',
  '{"cmd": "set", "resource": "Time<Virtual>", "value": {"context": {"paused": true}}}',
  "```",
  "",
  "## Tips",
  "",
  "- Always call `get_schema` before `set_resource` — field names must match exactly",
  "- `set_resource` is a merge patch — omitted fields keep their current values",
  "- Materials use `Material:<ModelName>` naming, e.g. `Material:TowerExtension`",
  "- Multi-model scenes have separate entities — `list_resources` shows all",
  "- `screenshot` returns a base64 PNG data URL — use it to verify every change",
  "",
].join("\n");

// ── WebMCP script — injected into index.html before </head> ─────────────────
// Registers all MCP tools via navigator.modelContext (W3C Draft, Chrome 145+).
// Maps MCP tool names to bridge commands that POST to /call on the same origin.
// Feature-gated: only runs when the browser exposes navigator.modelContext.

const WEBMCP_TOOL_MAP = JSON.stringify(
  manifest.tools.map((t) => ({
    name: t.name,
    description: t.description,
    inputSchema: t.inputSchema || { type: "object", properties: {} },
  })),
);

// The cmd mapping from MCP tool names → bridge {cmd, ...} payloads.
const WEBMCP_SCRIPT = `<script>
(function() {
  if (!navigator.modelContext) return;
  var ac = new AbortController();
  var toolMap = {
    list_resources: function()    { return {cmd:"list"}; },
    get_resource:   function(i)   { return {cmd:"get", resource:i.name}; },
    set_resource:   function(i)   { return {cmd:"set", resource:i.name, value:i.value}; },
    screenshot:     function()    { return {cmd:"screenshot"}; },
    get_schema:     function(i)   { return {cmd:"schema", name:i.name}; },
    load_shape:     function(i)   { return {cmd:"load_shape", name:i.name, data:i.data}; },
    save_shape:     function(i)   { return {cmd:"save_shape", name:i.name}; },
    list_shapes:    function()    { return {cmd:"list_shapes"}; },
    load_step:      function(i)   { return {cmd:"load_step", name:i.name, data:i.data}; },
    save_step:      function(i)   { return {cmd:"save_step", name:i.name}; },
    delete_shape:   function(i)   { return {cmd:"delete_shape", name:i.name}; },
    simulate_touch: function(i)   { return Object.assign({cmd:"simulate_touch"}, i); }
  };
  var tools = ${WEBMCP_TOOL_MAP};
  tools.forEach(function(t) {
    var mapFn = toolMap[t.name] || function(i) { return {cmd:t.name}; };
    navigator.modelContext.registerTool({
      name: t.name,
      description: t.description,
      inputSchema: t.inputSchema,
      execute: function(input) {
        return fetch("/call", {
          method: "POST",
          headers: {"Content-Type": "application/json"},
          body: JSON.stringify(mapFn(input || {}))
        }).then(function(r) { return r.json(); });
      }
    }, {signal: ac.signal});
  });
  window.addEventListener("pagehide", function() { ac.abort(); }, {once:true});
  console.log("[WebMCP] registered " + tools.length + " tools");
})();
</script>`;

// Lazy SHA-256 digest of SKILL_MD (computed once, cached).
let _skillDigest = null;
async function skillDigest() {
  if (!_skillDigest) {
    const buf = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(SKILL_MD),
    );
    _skillDigest =
      "sha256:" +
      [...new Uint8Array(buf)]
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");
  }
  return _skillDigest;
}

/**
 * Handle agent-readiness routes inline (not from R2).
 * Returns a Response or null (fall through to R2/DO).
 * All data derived from pmetra-manifest.json — single source of truth.
 */
async function agentRoutes(url) {
  const origin = url.origin;

  // ── robots.txt with Content Signals ──
  if (url.pathname === "/robots.txt") {
    return new Response(
      [
        "User-agent: *",
        "Allow: /",
        "Content-Signal: ai-train=no, search=yes, ai-input=yes",
        "",
        `Sitemap: ${origin}/sitemap.xml`,
        "",
      ].join("\n"),
      { headers: { "content-type": "text/plain; charset=utf-8" } },
    );
  }

  // ── sitemap.xml — all model variant URLs ──
  if (url.pathname === "/sitemap.xml") {
    const entries = [
      `  <url><loc>${origin}/</loc></url>`,
      ...manifest.models.map(
        (m) => `  <url><loc>${origin}/?model=${modelName(m)}</loc></url>`,
      ),
    ].join("\n");
    return new Response(
      [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">',
        entries,
        "</urlset>",
        "",
      ].join("\n"),
      { headers: { "content-type": "application/xml; charset=utf-8" } },
    );
  }

  // ── MCP Server Card (SEP-1649) ──
  if (
    url.pathname === "/.well-known/mcp/server-card.json" ||
    url.pathname === "/.well-known/mcp/server-cards.json" ||
    url.pathname === "/.well-known/mcp.json"
  ) {
    const card = {
      serverInfo: { name: manifest.name, version: manifest.version },
      description: manifest.description,
      url: `${origin}/call`,
      transport: { type: "streamable-http" },
      capabilities: { tools: true, resources: true },
    };
    const body =
      url.pathname === "/.well-known/mcp/server-cards.json" ? [card] : card;
    return Response.json(body);
  }

  // ── API Catalog (RFC 9727) ──
  if (url.pathname === "/.well-known/api-catalog") {
    return new Response(
      JSON.stringify({
        linkset: [
          {
            anchor: `${origin}/`,
            "service-desc": [
              {
                href: "/.well-known/mcp/server-card.json",
                type: "application/json",
              },
            ],
          },
        ],
      }),
      {
        headers: {
          "content-type": "application/linkset+json; charset=utf-8",
        },
      },
    );
  }

  // ── A2A Agent Card (Google A2A protocol) ──
  if (url.pathname === "/.well-known/agent-card.json") {
    return Response.json({
      name: manifest.name,
      description: manifest.description,
      version: manifest.version,
      url: origin,
      supportedInterfaces: [
        {
          url: `${origin}/call`,
          protocolBinding: "HTTP+JSON",
          protocolVersion: manifest.version,
        },
      ],
      capabilities: { streaming: false, pushNotifications: false },
      defaultInputModes: ["application/json"],
      defaultOutputModes: ["application/json", "image/png"],
      skills: manifest.skills.map((s) => ({
        id: s.id,
        name: s.name,
        description: s.description,
        tags: s.tags,
        examples: [
          "Set the tower height to 5.0",
          "Switch to the NURBS surface model",
          "Take a screenshot of the current viewport",
        ],
      })),
    });
  }

  // ── Agent Skills Discovery (agentskills.io v0.2.0) ──
  if (url.pathname === "/.well-known/agent-skills/index.json") {
    const digest = await skillDigest();
    return Response.json({
      $schema:
        "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
      skills: manifest.skills.map((s) => ({
        name: s.id,
        type: "skill-md",
        description: s.description,
        url: `/.well-known/agent-skills/${s.id}/SKILL.md`,
        digest,
      })),
    });
  }

  // ── Individual SKILL.md ──
  if (
    url.pathname === "/.well-known/agent-skills/cad-control/SKILL.md"
  ) {
    return new Response(SKILL_MD, {
      headers: { "content-type": "text/markdown; charset=utf-8" },
    });
  }

  // ── .well-known catch-all — proper 404, not SPA fallback ──
  if (url.pathname.startsWith("/.well-known/")) {
    return new Response("Not Found", { status: 404 });
  }

  return null;
}

// ── Markdown overview — shared by /docs and Accept: text/markdown ────────────

function buildOverviewMd(origin) {
  return [
    `# ${manifest.name}`,
    "",
    manifest.description,
    "",
    "## Models",
    "",
    ...manifest.models.flatMap((m) => {
      const n = modelName(m);
      const line = `- [${n}](${origin}/?model=${n})`;
      return m.description ? [line + " — " + m.description] : [line];
    }),
    "",
    "## API",
    "",
    `- \`POST ${origin}/call\` — send commands to the CAD engine`,
    `- \`GET ${origin}/health\` — server status`,
    `- [MCP Server Card](${origin}/.well-known/mcp/server-card.json)`,
    `- [Agent Card](${origin}/.well-known/agent-card.json)`,
    `- [Skill details](${origin}/.well-known/agent-skills/cad-control/SKILL.md)`,
    "",
    "## MCP Tools",
    "",
    ...manifest.tools.map((t) => `- **${t.name}** — ${t.description}`),
    "",
  ].join("\n");
}

// ── Durable Object: single-slot WebSocket broker ────────────────────────────
//
// Holds the WASM app's WebSocket connection. /call sends a command via the WS,
// waits for the reply (matched by _seq), and returns it as HTTP response.
// Single-slot: latest WASM connection wins (same as the local Rust broker).

export class PmetraBroker {
  constructor(state, env) {
    this.state = state;
    this.env = env;
    this.pendingCalls = new Map(); // seq → { resolve, timer }
    this.nextSeq = 1;
    // Restore WebSocket connections surviving hibernation.
    const sockets = this.state.getWebSockets();
    this.wasmSocket = sockets.length > 0 ? sockets[0] : null;
  }

  async fetch(request) {
    const url = new URL(request.url);

    // ── Health ─────────────────────────────────────────────────────────────
    if (url.pathname === "/health") {
      return Response.json({
        ok: true,
        wasm_connected: this.wasmSocket !== null,
      });
    }

    // ── Forward command to WASM app ───────────────────────────────────────
    if (url.pathname === "/call" && request.method === "POST") {
      if (!this.wasmSocket) {
        return Response.json(
          { ok: false, error: "WASM app not connected" },
          { status: 503 },
        );
      }

      const cmd = await request.json();
      const seq = this.nextSeq++;
      cmd._seq = seq;

      return new Promise((resolve) => {
        const timer = setTimeout(() => {
          this.pendingCalls.delete(seq);
          resolve(
            Response.json(
              { ok: false, error: "timeout (30s)" },
              { status: 504 },
            ),
          );
        }, 30_000);

        this.pendingCalls.set(seq, { resolve, timer });
        this.wasmSocket.send(JSON.stringify(cmd));
      });
    }

    // ── WebSocket upgrade (WASM app connects here) ───────────────────────
    if (request.headers.get("Upgrade") === "websocket") {
      // Close previous connection (single-slot).
      if (this.wasmSocket) {
        try {
          this.wasmSocket.close(1000, "replaced");
        } catch {
          /* already closed */
        }
      }
      const pair = new WebSocketPair();
      this.state.acceptWebSocket(pair[1]);
      this.wasmSocket = pair[1];
      return new Response(null, { status: 101, webSocket: pair[0] });
    }

    return new Response("Bad Request", { status: 400 });
  }

  // ── WebSocket Hibernation API callbacks ─────────────────────────────────

  async webSocketMessage(ws, message) {
    try {
      const text =
        typeof message === "string"
          ? message
          : new TextDecoder().decode(message);
      const msg = JSON.parse(text);
      const seq = msg._seq;
      if (seq !== undefined && this.pendingCalls.has(seq)) {
        const { resolve, timer } = this.pendingCalls.get(seq);
        clearTimeout(timer);
        this.pendingCalls.delete(seq);
        resolve(Response.json(msg));
      }
      // Messages without _seq are fire-and-forget (unsolicited events).
    } catch {
      /* ignore non-JSON */
    }
  }

  async webSocketClose(ws, code, reason, wasClean) {
    if (ws === this.wasmSocket) {
      this.wasmSocket = null;
    }
    // Reject all pending /call requests.
    for (const [, { resolve, timer }] of this.pendingCalls) {
      clearTimeout(timer);
      resolve(
        Response.json(
          { ok: false, error: "WASM app disconnected" },
          { status: 503 },
        ),
      );
    }
    this.pendingCalls.clear();
  }

  async webSocketError(ws, error) {
    await this.webSocketClose(ws, 1006, "error", false);
  }
}

// ── Main Worker: route to R2 or Durable Object ─────────────────────────────

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    // CORS preflight.
    if (request.method === "OPTIONS") {
      return new Response(null, { headers: corsHeaders() });
    }

    // Agent readiness routes (inline, not R2).
    const agentResp = await agentRoutes(url);
    if (agentResp) return agentResp;

    // Markdown overview — /docs (browseable) or / with Accept: text/markdown.
    if (
      url.pathname === "/docs" ||
      (url.pathname === "/" &&
        request.headers.get("accept")?.includes("text/markdown"))
    ) {
      return new Response(buildOverviewMd(url.origin), {
        headers: {
          "content-type": "text/markdown; charset=utf-8",
          link: LINK_HEADER,
        },
      });
    }

    // API + WebSocket → Durable Object (singleton "default" instance).
    const isWsUpgrade = request.headers.get("Upgrade") === "websocket";
    if (
      url.pathname === "/health" ||
      url.pathname === "/call" ||
      isWsUpgrade
    ) {
      const id = env.BROKER.idFromName("default");
      const stub = env.BROKER.get(id);
      const resp = await stub.fetch(request);
      // Add CORS headers to non-WebSocket responses.
      if (!isWsUpgrade) {
        return addCors(resp);
      }
      return resp;
    }

    // Everything else → static files from R2.
    return serveStatic(url, env);
  },
};

// ── Static file serving from R2 ─────────────────────────────────────────────

async function serveStatic(url, env) {
  let key = url.pathname.slice(1) || "index.html";

  const ext = "." + key.split(".").pop();
  const contentType = MIME_TYPES[ext] || "application/octet-stream";

  const object = await env.ASSETS.get(key);
  if (!object) {
    // SPA fallback — serve index.html for unknown paths.
    const fallback = await env.ASSETS.get("index.html");
    if (!fallback) return new Response("Not Found", { status: 404 });
    return new Response(
      await injectWebMcp(fallback),
      {
        headers: {
          "content-type": "text/html",
          "cache-control": "no-cache",
          link: LINK_HEADER,
        },
      },
    );
  }

  const headers = new Headers();
  headers.set("content-type", contentType);
  if (contentType === "text/html") {
    headers.set("link", LINK_HEADER);
  }
  // Cache hashed assets for a day, everything else for 60s.
  if (ext === ".wasm" || ext === ".js") {
    headers.set("cache-control", "public, max-age=86400");
    headers.set("vary", "Accept-Encoding");
  } else {
    headers.set("cache-control", "public, max-age=60");
  }

  // Inject WebMCP into HTML pages.
  if (contentType === "text/html") {
    return new Response(await injectWebMcp(object), { headers });
  }

  return new Response(object.body, { headers });
}

/**
 * Read an R2 object's body as text and inject the WebMCP <script> before </head>.
 * If </head> is not found, returns the original HTML unchanged.
 */
async function injectWebMcp(r2Object) {
  const html = await r2Object.text();
  if (!html.includes("</head>")) return html;
  return html.replace("</head>", WEBMCP_SCRIPT + "\n  </head>");
}

// ── CORS helpers ─────────────────────────────────────────────────────────────

function corsHeaders() {
  return {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
  };
}

function addCors(response) {
  const headers = new Headers(response.headers);
  for (const [k, v] of Object.entries(corsHeaders())) {
    headers.set(k, v);
  }
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}
