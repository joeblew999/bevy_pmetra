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
    return new Response(fallback.body, {
      headers: { "content-type": "text/html", "cache-control": "no-cache" },
    });
  }

  const headers = new Headers();
  headers.set("content-type", contentType);
  // Cache hashed assets for a day, everything else for 60s.
  if (ext === ".wasm" || ext === ".js") {
    headers.set("cache-control", "public, max-age=86400");
    headers.set("vary", "Accept-Encoding");
  } else {
    headers.set("cache-control", "public, max-age=60");
  }

  return new Response(object.body, { headers });
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
