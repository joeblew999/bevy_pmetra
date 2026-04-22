//! Rust MCP server for pmetra — standalone, no Node.js required.
//!
//! Three roles in one binary, one port:
//!
//!   ROLE 1 — WebSocket broker (port 9001, /ws)
//!     The WASM app connects to ws://localhost:9001/ws on startup.
//!     Keeps one live connection; routes replies by sequence number.
//!
//!   ROLE 2 — HTTP API (port 9001, /health and /call)
//!     Testable with curl. Forwards commands to WASM via the WS broker.
//!     Same endpoints as the Cloudflare Worker — tests work on both.
//!       GET  /health → {"ok":true,"wasm_connected":true}
//!       POST /call   → forward JSON body to WASM, return reply
//!
//!   ROLE 3 — MCP server (stdio, foreground)
//!     Speaks JSON-RPC over stdio. Claude Desktop / claude CLI connect here.
//!     Exposes: list_resources, get_resource, set_resource, get_schema, screenshot, etc.
//!
//!   ROLE 4 (optional) — Static file server (--features embed-ui)
//!     Serves the WASM app from embedded dist/ files. End users run one
//!     binary, open http://localhost:9001, and the CAD app loads.
//!
//! Usage:
//!   cargo run -p pmetra-mcp-server                        # dev: MCP + HTTP/WS
//!   just mcp-rs                                           # same
//!   just mcp-broker                                       # HTTP/WS only (no stdio)
//!   ./pmetra-mcp-server                                   # end user: open :9001 in browser
//!
//! Test:
//!   curl localhost:9001/health
//!   curl -X POST localhost:9001/call -d '{"cmd":"schema","name":"TowerExtension"}'
//!   curl -X POST localhost:9001/call -d '{"cmd":"list"}'
//!
//! Environment:
//!   WS_PORT   — HTTP/WS port (default: 9001)

use std::{
    collections::HashMap,
    sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}},
};

use axum::{
    Router,
    extract::{FromRequestParts, State, ws::{Message, WebSocket, WebSocketUpgrade}},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::ServerInfo,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tracing::{info, warn};

const DEFAULT_WS_PORT: u16 = 9001;

// ─── Embedded static files (optional, --features embed-ui) ───────────────────

#[cfg(feature = "embed-ui")]
#[derive(rust_embed::Embed)]
#[folder = "../dist/"]
struct Asset;

/// Map file extension to MIME type. Same list as the CF Worker — consistent behavior.
#[cfg(feature = "embed-ui")]
fn mime_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html",
        "js" => "application/javascript",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Serve embedded static files from dist/ (SPA fallback to index.html).
/// Without embed-ui feature: returns helpful message.
#[cfg(feature = "embed-ui")]
async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(content) = Asset::get(path) {
        return (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, mime_for_path(path))],
            content.data.to_vec(),
        ).into_response();
    }

    // SPA fallback — serve index.html for unknown paths.
    if let Some(content) = Asset::get("index.html") {
        return (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            content.data.to_vec(),
        ).into_response();
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

#[cfg(not(feature = "embed-ui"))]
async fn static_handler(_uri: axum::http::Uri) -> Response {
    (StatusCode::NOT_FOUND, "No UI embedded. Dev: use trunk serve. Release: build with --features embed-ui").into_response()
}

// ─── Shared broker state ───────────────────────────────────────────────────────

type WsSink = futures_util::stream::SplitSink<WebSocket, Message>;
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

#[derive(Clone)]
struct Broker {
    /// Sink to send messages to the WASM client. None when WASM is not connected.
    tx: Arc<Mutex<Option<WsSink>>>,
    /// Pending reply map: seq → oneshot sender.
    pending: PendingMap,
    /// Monotonic sequence counter.
    seq: Arc<AtomicU64>,
    /// Whether the WASM client is connected (lock-free read for /health).
    connected: Arc<AtomicBool>,
}

impl Broker {
    fn new() -> Self {
        Self {
            tx: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            seq: Arc::new(AtomicU64::new(0)),
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Send a command to the WASM bridge and wait for the reply (5s timeout).
    async fn call(&self, mut cmd: Value) -> Result<Value, String> {
        use futures_util::SinkExt;

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        cmd["_seq"] = Value::Number(seq.into());

        let (tx, rx) = oneshot::channel::<Value>();
        self.pending.lock().await.insert(seq, tx);

        // Send
        {
            let mut guard = self.tx.lock().await;
            let sink = guard.as_mut().ok_or("WASM app not connected")?;
            let text = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;
            sink.send(Message::Text(text.into()))
                .await
                .map_err(|e| e.to_string())?;
        }

        // Wait for reply (5s timeout)
        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| {
                let pending = self.pending.clone();
                tokio::spawn(async move { pending.lock().await.remove(&seq); });
                "reply timeout".to_string()
            })?
            .map_err(|_| "reply channel dropped".to_string())
    }

    /// Call and return just the "value" field (for get-style tools).
    async fn call_value(&self, cmd: Value) -> String {
        match self.call(cmd).await {
            Ok(r) => r.get("value").map(|v| v.to_string()).unwrap_or_else(|| r.to_string()),
            Err(e) => format!(r#"{{"error":"{e}"}}"#),
        }
    }

    /// Call and return the full reply (for mutation tools).
    async fn call_raw(&self, cmd: Value) -> String {
        match self.call(cmd).await {
            Ok(r) => r.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }
}

// ─── WebSocket handler (WASM app connects here) ──────────────────────────────

async fn ws_upgrade(ws: WebSocketUpgrade, State(broker): State<Broker>) -> Response {
    ws.on_upgrade(move |socket| handle_wasm_ws(socket, broker))
}

async fn handle_wasm_ws(socket: WebSocket, broker: Broker) {
    use futures_util::StreamExt;

    info!("WASM client connected");
    broker.connected.store(true, Ordering::Relaxed);

    let (sink, mut source) = socket.split();
    *broker.tx.lock().await = Some(sink);

    let pending = broker.pending.clone();
    let tx_ref = broker.tx.clone();
    let connected_ref = broker.connected.clone();

    while let Some(msg) = source.next().await {
        let Ok(Message::Text(text)) = msg else { continue };
        let Ok(val) = serde_json::from_str::<Value>(&text) else {
            warn!("WS: bad JSON from WASM");
            continue;
        };
        if let Some(seq) = val.get("_seq").and_then(|v| v.as_u64()) {
            if let Some(sender) = pending.lock().await.remove(&seq) {
                let _ = sender.send(val);
            }
        }
    }

    info!("WASM client disconnected");
    connected_ref.store(false, Ordering::Relaxed);
    *tx_ref.lock().await = None;
}

// ─── Root handler (WS upgrade OR index.html) ─────────────────────────────────

/// Root path serves two roles:
///   - WebSocket upgrade → WASM bridge connection (backward compat with ws://host:port)
///   - Normal GET        → index.html from embedded files (end-user UI)
async fn root_handler(
    State(broker): State<Broker>,
    req: axum::extract::Request,
) -> Response {
    // WebSocket upgrade takes priority.
    let is_ws = req.headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));

    if is_ws {
        // Re-parse as WebSocketUpgrade from the request parts.
        let (mut parts, _body) = req.into_parts();
        if let Ok(ws) = WebSocketUpgrade::from_request_parts(&mut parts, &broker).await {
            return ws.on_upgrade(move |socket| handle_wasm_ws(socket, broker));
        }
    }

    // Normal browser request → serve index.html.
    #[cfg(feature = "embed-ui")]
    if let Some(content) = Asset::get("index.html") {
        return (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/html")],
            content.data.to_vec(),
        ).into_response();
    }
    (StatusCode::OK, "pmetra MCP server running. Build with --features embed-ui to serve UI here.").into_response()
}

// ─── HTTP handlers ────────────────────────────────────────────────────────────

async fn health_handler(State(broker): State<Broker>) -> Json<Value> {
    Json(serde_json::json!({
        "ok": true,
        "wasm_connected": broker.connected.load(Ordering::Relaxed),
    }))
}

async fn call_handler(
    State(broker): State<Broker>,
    axum::Json(body): axum::Json<Value>,
) -> (StatusCode, Json<Value>) {
    match broker.call(body).await {
        Ok(reply) => (StatusCode::OK, Json(reply)),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ok": false, "error": e})),
        ),
    }
}

// ─── Tool parameter types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GetParams {
    /// Short name of the resource or component.
    /// Examples: "TowerExtension", "GlobalAmbientLight", "Material:TowerExtension"
    /// Use list_resources first to discover all available names.
    name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SetParams {
    /// Short name of the resource or component to patch.
    name: String,
    /// JSON object with fields to update. Omitted fields keep their current values.
    /// Examples:
    ///   {"tower_length": 3}
    ///   {"brightness": 1200}
    ///   {"LinearRgba": {"red":0.0,"green":0.0,"blue":0.1,"alpha":1.0}}
    ///   {"base_color":{"Srgba":{"red":1.0,"green":0.84,"blue":0.0,"alpha":1.0}},"metallic":1.0}
    ///   {"translation":[1.0,0.0,0.0],"scale":[2.0,2.0,2.0]}
    ///   {"context":{"paused":true}}
    value: Value,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct LoadShapeParams {
    /// Display name for the loaded shape, e.g. "cube".
    name: String,
    /// Full Truck JSON content (CompressedShell format).
    data: String,
    /// Optional position: {"translation": [x, y, z]}.
    transform: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct NameOnlyParams {
    /// Name of the shape (as used in load_shape/load_step).
    name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct LoadStepParams {
    /// Display name for the loaded model, e.g. "bracket".
    name: String,
    /// Full STEP file content (ISO-10303-21 format).
    data: String,
    /// Optional position: {"translation": [x, y, z]}.
    transform: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SchemaParams {
    /// Type name to get the schema for, e.g. "TowerExtension".
    /// Use "*" or omit to get schemas for all registered types.
    name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SimulateTouchParams {
    /// X coord of touchstart in canvas-local pixels (from canvas top-left).
    x: f64,
    /// Y coord of touchstart in canvas-local pixels.
    y: f64,
    /// X coord of touchend. Omit for a tap (same as start).
    end_x: Option<f64>,
    /// Y coord of touchend. Omit for a tap.
    end_y: Option<f64>,
    /// Total drag duration in milliseconds. Default 300.
    duration_ms: Option<f64>,
}

// ─── MCP server ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct PmetraServer {
    broker: Broker,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl PmetraServer {
    fn new(broker: Broker) -> Self {
        Self { broker, tool_router: Self::tool_router() }
    }

    /// List all resources, components, and materials currently exposed by the
    /// running pmetra WASM app. Returns a JSON array of short names.
    #[tool(name = "list_resources")]
    async fn list_resources(&self) -> String {
        self.broker.call_value(serde_json::json!({ "cmd": "list" })).await
    }

    /// Get the current JSON value of a named resource, component, or material.
    #[tool(name = "get_resource")]
    async fn get_resource(&self, params: Parameters<GetParams>) -> String {
        self.broker.call_value(serde_json::json!({ "cmd": "get", "resource": params.0.name })).await
    }

    /// Patch a resource, component, or material. Only provided fields are changed.
    /// Triggers geometry rebuild for CAD param types (TowerExtension, ExpNurbs, etc.).
    #[tool(name = "set_resource")]
    async fn set_resource(&self, params: Parameters<SetParams>) -> String {
        self.broker.call_raw(serde_json::json!({ "cmd": "set", "resource": params.0.name, "value": params.0.value })).await
    }

    /// Capture a PNG screenshot of the current Bevy 3D viewport.
    /// Returns a base64 data URL: "data:image/png;base64,...".
    #[tool(name = "screenshot")]
    async fn screenshot(&self) -> String {
        self.broker.call_value(serde_json::json!({ "cmd": "screenshot" })).await
    }

    /// Load a Truck JSON shape into the scene as a rendered mesh.
    #[tool(name = "load_shape")]
    async fn load_shape(&self, params: Parameters<LoadShapeParams>) -> String {
        let mut cmd = serde_json::json!({ "cmd": "load_shape", "name": params.0.name, "data": params.0.data });
        if let Some(t) = &params.0.transform { cmd["transform"] = t.clone(); }
        self.broker.call_raw(cmd).await
    }

    /// Save a loaded TruckModel entity back to Truck JSON format.
    #[tool(name = "save_shape")]
    async fn save_shape(&self, params: Parameters<NameOnlyParams>) -> String {
        self.broker.call_raw(serde_json::json!({ "cmd": "save_shape", "name": params.0.name })).await
    }

    /// List all loaded Truck shapes (both JSON and STEP).
    #[tool(name = "list_shapes")]
    async fn list_shapes(&self) -> String {
        self.broker.call_raw(serde_json::json!({ "cmd": "list_shapes" })).await
    }

    /// Load a STEP file into the scene as rendered mesh(es). View-only.
    #[tool(name = "load_step")]
    async fn load_step(&self, params: Parameters<LoadStepParams>) -> String {
        let mut cmd = serde_json::json!({ "cmd": "load_step", "name": params.0.name, "data": params.0.data });
        if let Some(t) = &params.0.transform { cmd["transform"] = t.clone(); }
        self.broker.call_raw(cmd).await
    }

    /// Save a loaded StepModel entity's raw STEP data.
    #[tool(name = "save_step")]
    async fn save_step(&self, params: Parameters<NameOnlyParams>) -> String {
        self.broker.call_raw(serde_json::json!({ "cmd": "save_step", "name": params.0.name })).await
    }

    /// Delete a loaded shape — despawns entity and removes from localStorage.
    #[tool(name = "delete_shape")]
    async fn delete_shape(&self, params: Parameters<NameOnlyParams>) -> String {
        self.broker.call_raw(serde_json::json!({ "cmd": "delete_shape", "name": params.0.name })).await
    }

    /// Get the field-level schema for a registered Bevy type. Use before
    /// set_resource to discover valid field names. Use "*" for all types.
    #[tool(name = "get_schema")]
    async fn get_schema(&self, params: Parameters<SchemaParams>) -> String {
        let name = params.0.name.unwrap_or_else(|| "*".to_string());
        self.broker.call_value(serde_json::json!({ "cmd": "schema", "name": name })).await
    }

    /// Dispatch a synthetic touch on the canvas. Coords are pixels from the
    /// canvas top-left. Omit end_x/end_y for a tap; provide them for a drag.
    #[tool(name = "simulate_touch")]
    async fn simulate_touch(&self, params: Parameters<SimulateTouchParams>) -> String {
        let dur = params.0.duration_ms.unwrap_or(300.0);
        let cmd = serde_json::json!({
            "cmd": "simulate_touch",
            "x": params.0.x, "y": params.0.y,
            "end_x": params.0.end_x.unwrap_or(params.0.x),
            "end_y": params.0.end_y.unwrap_or(params.0.y),
            "duration_ms": dur,
        });
        let result = self.broker.call_raw(cmd).await;
        tokio::time::sleep(std::time::Duration::from_millis((dur as u64) + 50)).await;
        result
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PmetraServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::default()
    }
}

// ─── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("pmetra_mcp_server=info".parse()?)
        )
        .with_writer(std::io::stderr)
        .init();

    let ws_port: u16 = std::env::var("WS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WS_PORT);

    let broker = Broker::new();

    // HTTP + WS server on one port.
    //   /health, /call   → API
    //   /ws              → WebSocket (WASM app connects here)
    //   /                → WebSocket upgrade OR index.html (dual-purpose)
    //   /*               → embedded static files (with embed-ui) or 404
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/call", post(call_handler))
        .route("/ws", get(ws_upgrade))
        .route("/", get(root_handler))
        .fallback(static_handler)
        .with_state(broker.clone());

    let addr = format!("0.0.0.0:{ws_port}");
    let listener = tokio::net::TcpListener::bind(&addr).await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));

    #[cfg(feature = "embed-ui")]
    info!("http://{addr} — WASM app + API + WS (open in browser)");
    #[cfg(not(feature = "embed-ui"))]
    info!("http://{addr} — API + WS (use trunk serve for UI)");

    let http_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // MCP over stdio — serve() blocks until stdin closes (MCP client disconnects).
    // Errors here (e.g. no piped client) are non-fatal; the HTTP/WS server keeps running.
    info!("pmetra MCP server (Rust) ready — waiting for MCP client on stdio");
    let server = PmetraServer::new(broker);
    match server.serve(stdio()).await {
        Ok(service) => {
            tokio::select! {
                _ = service.waiting() => info!("MCP client disconnected"),
                _ = http_handle => {}
            }
        }
        Err(e) => {
            info!("MCP stdio not connected ({e}) — running as HTTP/WS-only server");
            http_handle.await.ok();
        }
    }
    Ok(())
}
